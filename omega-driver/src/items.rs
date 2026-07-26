//! The one global, item-granular query behind same-module resolution,
//! cross-module resolution, and generic instantiation alike.
//!
//! Every top-level item is its own independent, memoized query, so one bad
//! item never poisons an unrelated sibling's, and declaration order never
//! matters anywhere.

use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::analysis::{Analyzer, PendingSpecMethod, item_id_span, item_visibility};
use omega_analyzer::annotations::ResolvedAnnotations;
use omega_analyzer::checked::{CheckedItem, Storage};
use omega_analyzer::error::AnalysisWarning;
use omega_analyzer::resolved_type::{
    ResolvedEnumType, ResolvedFunctionType, ResolvedMethod, ResolvedSpecType, ResolvedStructType, ResolvedType,
    ResolvedUnionType,
};
use omega_analyzer::resolver::{ResolveError, ResolvedItem};
use omega_diagnostics::Span;
use omega_hir::{HirGenericParam, HirId, HirItem, SYNTHETIC_MODULE};
use omega_parser::prelude::{Ident, Visibility};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// One item query's identity: its owning module, its name, and the concrete
/// type arguments it was instantiated with -- empty for an ordinary,
/// non-generic item, or a generic item's instantiation-specific substitution
/// (`List<u32>`'s `[u32]`). There is no architectural difference between "an
/// ordinary item" and "a generic instantiation of one": both are this one key
/// shape, just with different `type_args`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ItemKey {
    pub module: ModulePath,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
}

impl ItemKey {
    pub fn new(module: &[Ident], name: &Ident, type_args: &[ResolvedType]) -> Self {
        Self { module: module.to_vec(), name: name.clone(), type_args: type_args.to_vec() }
    }

    /// Whether this key addresses a *specific instantiation* of a generic
    /// template rather than an ordinary item. Instantiations get their own
    /// synthetic identities, are body-checked on demand instead of by
    /// `compile`'s static sweep, and are compiled locally even when the
    /// template they came from is extern-owned.
    pub fn is_instantiation(&self) -> bool {
        !self.type_args.is_empty()
    }

    fn failed(&self) -> ResolveError {
        ResolveError::ItemFailed { module: self.module.clone(), item: self.name.clone() }
    }
}

/// A spec's own canonical identity -- deliberately **not** an [`ItemKey`]: a
/// spec's cell content never varies by type arguments (its
/// `functions`/`dependencies` stay raw until a concrete implementor is
/// known), so there is exactly one canonical cell per spec regardless of how
/// many different concrete args reference it.
type SpecKey = (ModulePath, Ident);

/// One overload candidate's identity: an overload group's candidates all
/// share a name, so only their position in the module distinguishes them.
type OverloadKey = (ModulePath, usize);

/// A query's memoized state -- the white/gray/black cycle guard. An item
/// whose signature collection is already on the call stack is gray, and a
/// second request for it before the first finishes is either fine (an
/// indirect, pointer reference) or a genuine by-value cycle.
pub(crate) enum QueryState {
    InProgress,
    Done,
}

/// One item's successfully resolved signature, with the declared visibility
/// every *later* reference to it is checked against.
struct ResolvedEntry {
    visibility: Visibility,
    item: ResolvedItem,
}

/// One item's fully checked body plus whatever warnings checking it produced.
pub(crate) struct CheckedBody {
    pub item: CheckedItem,
    pub warnings: Vec<AnalysisWarning>,
}

impl CheckedBody {
    pub fn clone_of(&self) -> Self {
        Self { item: self.item.clone(), warnings: self.warnings.clone() }
    }
}

/// Every struct/enum/union's shared identity cell, decoupled from any one
/// module's analysis: created the moment *anyone* (same-module or foreign)
/// first asks about a given type, independent of whether its own module has
/// started resolving it. This is what lets an indirect (pointer) reference to
/// a type that's mid-collection be served immediately, without needing
/// exclusive access to whatever is currently building it.
///
/// All three are `IndexMap`s so every whole-cache walk visits cells in the
/// (deterministic) order they were created, rather than a `HashMap`'s
/// per-process-random order -- what keeps repeated builds of identical source
/// byte-for-byte identical.
#[derive(Default)]
pub(crate) struct TypeCells {
    structs: IndexMap<ItemKey, Rc<RefCell<ResolvedStructType>>>,
    enums: IndexMap<ItemKey, Rc<RefCell<ResolvedEnumType>>>,
    unions: IndexMap<ItemKey, Rc<RefCell<ResolvedUnionType>>>,
}

impl TypeCells {
    /// Gets (or creates) `key`'s struct cell. Always called with a real `id`
    /// (the struct's own `HirId`, or a freshly minted synthetic one for an
    /// instantiation) from `compute_item`, right before that same struct is
    /// marked `InProgress` and analyzed -- so nothing can ever observe a
    /// missing cell for a type that is actually in progress.
    pub fn struct_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedStructType>> {
        self.structs
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedStructType {
                    id,
                    name: key.name.clone(),
                    module_path: key.module.clone(),
                    type_args: key.type_args.clone(),
                    fields: vec![],
                    functions: vec![],
                    layout: Default::default(),
                    suppress: vec![],
                }))
            })
            .clone()
    }

    /// The enum counterpart of [`Self::struct_cell`]. The placeholder's tag
    /// defaults to the implicit `u16`; `signature_of_enum` patches the real
    /// shape in.
    pub fn enum_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedEnumType>> {
        self.enums
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedEnumType {
                    id,
                    name: key.name.clone(),
                    module_path: key.module.clone(),
                    type_args: key.type_args.clone(),
                    tag_type: ResolvedType::U16,
                    header: vec![],
                    dynamic_fields: vec![],
                    variants: vec![],
                    functions: vec![],
                    layout: Default::default(),
                    suppress: vec![],
                }))
            })
            .clone()
    }

    /// The union counterpart of [`Self::struct_cell`].
    pub fn union_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedUnionType>> {
        self.unions
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedUnionType {
                    id,
                    name: key.name.clone(),
                    module_path: key.module.clone(),
                    type_args: key.type_args.clone(),
                    fields: vec![],
                    functions: vec![],
                    suppress: vec![],
                }))
            })
            .clone()
    }

    /// `key`'s already-created struct cell. Phase 2 only ever asks for a cell
    /// phase 1 built, so a miss is a driver bug.
    pub fn expect_struct(&self, key: &ItemKey) -> Rc<RefCell<ResolvedStructType>> {
        self.structs.get(key).expect("cell was created when the signature resolved").clone()
    }

    pub fn expect_enum(&self, key: &ItemKey) -> Rc<RefCell<ResolvedEnumType>> {
        self.enums.get(key).expect("cell was created when the signature resolved").clone()
    }

    pub fn expect_union(&self, key: &ItemKey) -> Rc<RefCell<ResolvedUnionType>> {
        self.unions.get(key).expect("cell was created when the signature resolved").clone()
    }

    /// `key`'s existing cell as a type, whichever of the three kinds it is --
    /// `None` when `key` has no cell at all (i.e. it isn't an aggregate).
    pub fn resolved_type(&self, key: &ItemKey) -> Option<ResolvedType> {
        if let Some(cell) = self.structs.get(key) {
            return Some(ResolvedType::Struct(cell.clone()));
        }
        if let Some(cell) = self.enums.get(key) {
            return Some(ResolvedType::Enum { cell: cell.clone(), variant: None });
        }
        self.unions.get(key).map(|cell| ResolvedType::Union(cell.clone()))
    }

    pub fn structs(&self) -> impl Iterator<Item = (&ItemKey, &Rc<RefCell<ResolvedStructType>>)> {
        self.structs.iter()
    }

    pub fn enums(&self) -> impl Iterator<Item = (&ItemKey, &Rc<RefCell<ResolvedEnumType>>)> {
        self.enums.iter()
    }

    pub fn unions(&self) -> impl Iterator<Item = (&ItemKey, &Rc<RefCell<ResolvedUnionType>>)> {
        self.unions.iter()
    }

    /// Every cell's own method list, whichever kind it came from -- the three
    /// are indistinguishable to a caller that only wants methods.
    pub fn all_methods(&self) -> impl Iterator<Item = (&ItemKey, Vec<(Ident, ResolvedMethod)>)> {
        self.structs
            .iter()
            .map(|(key, cell)| (key, cell.borrow().functions.clone()))
            .chain(self.enums.iter().map(|(key, cell)| (key, cell.borrow().functions.clone())))
            .chain(self.unions.iter().map(|(key, cell)| (key, cell.borrow().functions.clone())))
    }
}

/// Every item query's memoized state and results.
#[derive(Default)]
pub(crate) struct ItemQueries {
    state: HashMap<ItemKey, QueryState>,
    /// Every item that finished its query successfully -- absent for one
    /// that's `Done` but failed; the real diagnostics for those live in the
    /// diagnostic sink instead. `IndexMap`, because the end-of-compile sweep
    /// for extern-owned references walks this whole map and its order reaches
    /// codegen: insertion (resolution) order is deterministic, a `HashMap`'s
    /// is per-process-random.
    resolved: IndexMap<ItemKey, ResolvedEntry>,
    pub cells: TypeCells,
    spec_cells: HashMap<SpecKey, Rc<RefCell<ResolvedSpecType>>>,
    /// The `cells` counterpart of `state`, at spec granularity -- spec
    /// declaration bypasses `ensure_item` entirely, so it needs its own guard
    /// or a genuine `spec A : B; spec B : A;` cycle would recurse forever.
    spec_state: HashMap<SpecKey, QueryState>,
    /// Every free (non-method) function's resolved `@inline`/`@mangling`/
    /// `@suppress`, keyed by its own declaration id. Methods carry theirs
    /// inline on `ResolvedMethod`; free functions have no equivalent
    /// per-declaration record of their own (`ResolvedItem` is shared with
    /// globals/externs), so they get this sibling cache. Resolving
    /// annotations once, at signature time, is what lets an extern-owned
    /// function's annotations be seen without ever checking its body.
    pub function_annotations: HashMap<HirId, ResolvedAnnotations>,
    /// One overload candidate's resolved signature, memoized by position
    /// rather than name. Unlike the aggregate cells, a function signature has
    /// no self-referential-cycle risk of its own (nothing ever embeds a
    /// function *by value* the way a struct field embeds another struct), so
    /// this is a plain memo with no `InProgress` guard. `IndexMap` for the
    /// same reason `resolved` above is one.
    pub overload_signatures: IndexMap<OverloadKey, ResolvedFunctionType>,
    pub overload_bodies: HashMap<OverloadKey, CheckedBody>,
    /// Every generic instantiation's checked body, discovered and computed on
    /// demand (see `Driver::ensure_item`) rather than by `compile`'s static
    /// per-module sweep, since instantiations aren't statically enumerable.
    /// Merged into their originating module only after both phases finish --
    /// an instantiation can be discovered at any point during either, so
    /// nothing may assume this map is complete any earlier. `IndexMap` for
    /// deterministic (discovery-order) merging.
    pub generic_instantiations: IndexMap<ItemKey, CheckedBody>,
    /// Every spec-default-method instantiation an implementor's `implements`
    /// clause needed (no own override), queued during phase 1 and drained
    /// during phase 2, once that implementor's own body is checked.
    pub pending_spec_methods: HashMap<ItemKey, Vec<PendingSpecMethod>>,
    /// Counter behind every synthetic `HirId`. Always paired with
    /// `SYNTHETIC_MODULE`, a module id the lowerer never produces, so these
    /// can never collide with a real per-file id.
    next_synthetic_id: u32,
}

impl ItemQueries {
    /// A fresh, globally unique `HirId` for something with no HIR node of its
    /// own: a generic instantiation's identity, or a spec-default method
    /// instantiated for a concrete implementor.
    pub fn fresh_synthetic_id(&mut self) -> HirId {
        let id = HirId { module: SYNTHETIC_MODULE, local: self.next_synthetic_id };
        self.next_synthetic_id += 1;
        id
    }

    /// **Identity is decided exactly once, here, per fresh key, and never
    /// again**: an ordinary item keeps its own declared `HirId`, while a
    /// generic instantiation gets a fresh synthetic one. Both the cells and
    /// the body phase read the decided id back out rather than recomputing
    /// it, so `List<u32>` and `List<i64>` are guaranteed genuinely distinct
    /// types/symbols with no risk of drift between the two phases.
    pub fn identity_for(&mut self, key: &ItemKey, declared: HirId) -> HirId {
        if key.is_instantiation() { self.fresh_synthetic_id() } else { declared }
    }

    /// [`Self::identity_for`] over an item's whole method list.
    pub fn method_identities(
        &mut self,
        key: &ItemKey,
        declared: impl IntoIterator<Item = HirId>,
    ) -> Vec<HirId> {
        declared.into_iter().map(|id| self.identity_for(key, id)).collect()
    }

    /// Whether `key` is already resolved, still being resolved, or untouched.
    pub fn state(&self, key: &ItemKey) -> Option<&QueryState> {
        self.state.get(key)
    }

    /// Marks `key` as being resolved right now -- the gray state every cycle
    /// check keys off.
    pub fn begin(&mut self, key: &ItemKey) {
        self.state.insert(key.clone(), QueryState::InProgress);
    }

    /// Marks `key` resolved, recording its signature when it succeeded. A
    /// failed query stays `Done` but absent, which is exactly what tells a
    /// later reference "this already failed" apart from "never asked".
    pub fn finish(&mut self, key: &ItemKey, visibility: Visibility, item: Option<&ResolvedItem>) {
        self.state.insert(key.clone(), QueryState::Done);
        if let Some(item) = item {
            self.resolved.insert(key.clone(), ResolvedEntry { visibility, item: item.clone() });
        }
    }

    /// A finished query's outcome -- `None` when it finished by failing.
    pub fn finished(&self, key: &ItemKey) -> Option<(Visibility, ResolvedItem)> {
        self.resolved.get(key).map(|entry| (entry.visibility, entry.item.clone()))
    }

    pub fn spec_cell(&self, key: &SpecKey) -> Option<Rc<RefCell<ResolvedSpecType>>> {
        self.spec_cells.get(key).cloned()
    }

    /// [`Self::state`]'s spec-granular counterpart.
    pub fn spec_state(&self, key: &SpecKey) -> Option<&QueryState> {
        self.spec_state.get(key)
    }

    pub fn begin_spec(&mut self, key: &SpecKey) {
        self.spec_state.insert(key.clone(), QueryState::InProgress);
    }

    /// [`Self::finish`]'s spec-granular counterpart -- same "`Done` but
    /// absent means it failed" convention.
    pub fn finish_spec(&mut self, key: &SpecKey, cell: Option<&Rc<RefCell<ResolvedSpecType>>>) {
        self.spec_state.insert(key.clone(), QueryState::Done);
        if let Some(cell) = cell {
            self.spec_cells.insert(key.clone(), cell.clone());
        }
    }

    /// One item's already-resolved signature. Phase 2 only runs after phase 1
    /// produced it, so a miss is a driver bug.
    pub fn expect_resolved(&self, key: &ItemKey) -> &ResolvedItem {
        &self.resolved.get(key).expect("every signature is resolved before its body is checked").item
    }

    /// Every resolved item's key paired with its signature -- the whole-cache
    /// walk `collect_extern_functions` needs.
    pub fn resolved_items(&self) -> impl Iterator<Item = (&ItemKey, &ResolvedItem)> {
        self.resolved.iter().map(|(key, entry)| (key, &entry.item))
    }
}

impl Driver {
    /// `exposed`/`internal`/(default private)'s access decision --
    /// `declaring`/`accessor` are absolute module paths. `Internal` is
    /// package-wide (same root segment), Rust `pub(crate)`-style, rather than
    /// the narrower "declaring module and its descendants only".
    pub(crate) fn visibility_allows(visibility: Visibility, declaring: &[Ident], accessor: &[Ident]) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Internal => declaring.first() == accessor.first(),
            Visibility::Private => declaring == accessor,
        }
    }

    /// One item's declared visibility, read straight off its HIR -- the
    /// property of a *declaration*, so it's identical for every instantiation
    /// of a generic template and needs no resolution to answer.
    pub(crate) fn declared_visibility(&mut self, module_path: &[Ident], name: &Ident) -> Option<Visibility> {
        let index = self.local_item_index(module_path, name).ok()?;
        self.modules.parsed(module_path).hir.items.get(index).map(item_visibility)
    }

    /// Gates a resolved item behind its own declared visibility. `bypass`
    /// (the `hidden` modifier) suppresses only this one check; it never
    /// affects what is cached.
    fn gate_visibility(
        item: ResolvedItem,
        visibility: Visibility,
        key: &ItemKey,
        accessor: &[Ident],
        bypass: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        if bypass || Self::visibility_allows(visibility, &key.module, accessor) {
            Ok(item)
        } else {
            Err(ResolveError::NotVisible { module: key.module.clone(), item: key.name.clone() })
        }
    }

    /// What a request for an item that is *already being resolved* gets.
    ///
    /// A pointer reference never needs its pointee's layout, so it can be
    /// served straight from the type's cell. Everything else is a genuine
    /// cycle: a by-value reference closes an infinite-size type, and an
    /// *import* (always indirect, whatever it names) looping back on an
    /// in-progress non-type item is a real mutual item-import cycle.
    fn in_progress_result(&self, key: &ItemKey, indirect: bool) -> Result<ResolvedItem, ResolveError> {
        if !indirect {
            return Err(ResolveError::RecursiveTypeWithoutIndirection {
                module: key.module.clone(),
                item: key.name.clone(),
            });
        }
        match self.items.cells.resolved_type(key) {
            Some(r#type) => Ok(ResolvedItem::Type(r#type)),
            None => Err(ResolveError::Cycle(vec![key.module.clone()])),
        }
    }

    /// The one global query behind same-module resolution, cross-module
    /// resolution, and generic instantiation alike (see
    /// `ModuleResolver::resolve_item`). A name already `Done` is a cache hit
    /// (successful or not); one found `InProgress` is either a legitimate
    /// indirect reference or a genuine cycle; anything else is analyzed right
    /// here, on the spot, before this returns.
    pub(crate) fn ensure_item(
        &mut self,
        accessor_module_path: &[Ident],
        module_path: &[Ident],
        name: &Ident,
        type_args: &[ResolvedType],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let key = ItemKey::new(module_path, name, type_args);

        match self.items.state(&key) {
            Some(QueryState::Done) => {
                let Some((visibility, item)) = self.items.finished(&key) else { return Err(key.failed()) };
                return Self::gate_visibility(item, visibility, &key, accessor_module_path, bypass);
            }
            Some(QueryState::InProgress) => return self.in_progress_result(&key, indirect),
            None => {}
        }

        let index = self.local_item_index(module_path, name)?;
        let generic_params = self.item_generics(module_path, name)?;
        if generic_params.len() != type_args.len() {
            return Err(ResolveError::GenericArgCountMismatch {
                module: module_path.to_vec(),
                item: name.clone(),
                expected: generic_params.len(),
                found: type_args.len(),
            });
        }
        if generic_params.iter().any(|g| g.bound.is_some()) {
            self.check_generic_bounds(&key, index, &generic_params, type_args)?;
        }

        let visibility = self.declared_visibility(module_path, name).expect("just indexed by local_item_index");
        let generics: Vec<Ident> = generic_params.iter().map(|g| g.ident.clone()).collect();

        self.items.begin(&key);
        let result = self.compute_item(&key, index, &generics);
        self.items.finish(&key, visibility, result.as_ref().ok());

        // A genuine instantiation's body is checked right here, immediately
        // after its own signature is marked `Done` (never while it's still
        // `InProgress`). That ordering is exactly why an ordinary same-module
        // recursive call never hits the `InProgress` branch above -- its
        // signature is always `Done` before its body is checked -- and
        // triggering here preserves the same invariant for a recursive
        // generic call, which `compile`'s static sweep could never enumerate.
        if result.is_ok() && key.is_instantiation() {
            self.check_generic_instantiation_body(&key, index);
        }

        Self::gate_visibility(result?, visibility, &key, accessor_module_path, bypass)
    }

    /// Checks every bound generic parameter (`T: Animal`) against the
    /// concrete argument it was instantiated with. Skipped entirely (not even
    /// called) for the common all-unbound case, so an ordinary duck-typed
    /// generic pays nothing. A resolution failure *inside* a bound (a typo'd
    /// spec name, say) is an ordinary recorded error and fails soft with
    /// `ItemFailed`; a bound that resolved fine but isn't satisfied is the
    /// real, structured `SpecNotImplemented`.
    fn check_generic_bounds(
        &mut self,
        key: &ItemKey,
        index: usize,
        generic_params: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Result<(), ResolveError> {
        let hir = self.modules.hir(&key.module);
        let owner = item_id_span(&hir.items[index]);
        let substitution: Vec<(Ident, ResolvedType)> =
            generic_params.iter().map(|g| g.ident.clone()).zip(type_args.iter().cloned()).collect();

        for (param, concrete) in generic_params.iter().zip(type_args) {
            let Some(bound) = param.bound.clone() else { continue };
            let run = self.with_analyzer(&key.module, &substitution, owner, |analyzer| {
                analyzer.check_generic_bound(owner.0, owner.1, &bound, concrete)
            });
            if run.failed {
                return Err(key.failed());
            }
            if let Some(Err((spec, missing))) = run.result {
                return Err(ResolveError::SpecNotImplemented { type_name: concrete.to_string(), spec, missing });
            }
        }
        Ok(())
    }

    /// Resolves one item's signature -- the work `ensure_item` defers to the
    /// first time a name is requested. Each kind gets its own throwaway
    /// `Analyzer`, seeded with the instantiation's substitution.
    fn compute_item(&mut self, key: &ItemKey, index: usize, generics: &[Ident]) -> Result<ResolvedItem, ResolveError> {
        let hir = self.modules.hir(&key.module);
        let item = &hir.items[index];
        let module = &key.module;
        let substitution: Vec<(Ident, ResolvedType)> =
            generics.iter().cloned().zip(key.type_args.iter().cloned()).collect();

        let resolved = match item {
            HirItem::Declaration(decl) => self
                .analyze(module, &substitution, (decl.id, decl.span), |a| {
                    a.analyze_declaration(decl, Storage::Global)
                })
                .map(|c| ResolvedItem::Value { r#type: c.r#type, storage: Storage::Global, decl_id: c.id }),

            HirItem::ExternDeclaration(decl) => self
                .analyze(module, &substitution, (decl.id, decl.span), |a| a.analyze_extern_decl(decl))
                .map(|c| {
                    let storage = match c.r#type {
                        ResolvedType::Function(_) => Storage::Function,
                        _ => Storage::Global,
                    };
                    ResolvedItem::Value { r#type: c.r#type, storage, decl_id: c.id }
                }),

            HirItem::FunctionDefinition(f) => {
                let id = self.items.identity_for(key, f.id);
                self.analyze(module, &substitution, (f.id, f.span), |a| a.collect_function_signature(f)).map(
                    |(fn_type, annotations)| {
                        self.items.function_annotations.insert(id, annotations);
                        ResolvedItem::Value {
                            r#type: ResolvedType::Function(fn_type),
                            storage: Storage::Function,
                            decl_id: id,
                        }
                    },
                )
            }

            HirItem::Struct(s) => {
                let id = self.items.identity_for(key, s.id);
                let cell = self.items.cells.struct_cell(key, id);
                let method_ids = self.items.method_identities(key, s.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Struct(cell.clone());
                self.compute_aggregate(key, (s.id, s.span), &substitution, self_type, method_ids, |a, ids| {
                    a.signature_of_struct(s, &cell, ids)
                })
            }

            HirItem::Enum(e) => {
                let id = self.items.identity_for(key, e.id);
                let cell = self.items.cells.enum_cell(key, id);
                let method_ids = self.items.method_identities(key, e.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Enum { cell: cell.clone(), variant: None };
                self.compute_aggregate(key, (e.id, e.span), &substitution, self_type, method_ids, |a, ids| {
                    a.signature_of_enum(e, &cell, ids)
                })
            }

            HirItem::Union(u) => {
                let id = self.items.identity_for(key, u.id);
                let cell = self.items.cells.union_cell(key, id);
                let method_ids = self.items.method_identities(key, u.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Union(cell.clone());
                self.compute_aggregate(key, (u.id, u.span), &substitution, self_type, method_ids, |a, ids| {
                    a.signature_of_union(u, &cell, ids)
                })
            }

            // A spec's cell is genuinely args-independent, so its
            // construction is fully delegated (and its own diagnostics
            // recorded) inside `resolve_spec_declaration`. This arm exists
            // only to serve the ordinary, already-arg-count-validated
            // reference path.
            HirItem::Spec(_) => {
                let absolute: Vec<Ident> = module.iter().cloned().chain([key.name.clone()]).collect();
                self.resolve_spec_declaration(&absolute)?.map(|cell| ResolvedItem::Type(ResolvedType::Spec(cell)))
            }

            HirItem::Import(_) => unreachable!("imports are never indexed into a module's items"),
        };

        resolved.ok_or_else(|| key.failed())
    }

    /// The shared spine of `compute_item`'s struct/enum/union arms: bind
    /// `Self` to the cell, resolve the signature, and queue whatever
    /// spec-default methods the `implements` clause left for phase 2.
    ///
    /// The cell is created by the caller *before* this runs, so a self- or
    /// mutually-referencing pointer field hit during field resolution finds
    /// it already there (`in_progress_result` serves it).
    fn compute_aggregate(
        &mut self,
        key: &ItemKey,
        owner: (HirId, Span),
        substitution: &[(Ident, ResolvedType)],
        self_type: ResolvedType,
        method_ids: Vec<HirId>,
        signature: impl FnOnce(&mut Analyzer, &[HirId]) -> (Option<()>, Vec<PendingSpecMethod>),
    ) -> Option<ResolvedItem> {
        let mut substitution = substitution.to_vec();
        substitution.push((Ident("Self".to_string()), self_type.clone()));

        let (ok, pending) =
            self.analyze(&key.module, &substitution, owner, |analyzer| signature(analyzer, &method_ids));
        ok?;
        self.items.pending_spec_methods.insert(key.clone(), pending);
        Some(ResolvedItem::Type(self_type))
    }

    /// A spec's canonical, args-independent declaration (see
    /// `ModuleResolver::spec_declaration`), cycle-guarded at spec
    /// granularity. `Ok(None)` for anything that isn't a spec at all --
    /// including a name that doesn't resolve -- deferring that diagnosis to
    /// the ordinary reference path, which re-derives it identically.
    pub(crate) fn resolve_spec_declaration(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError> {
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        let key: SpecKey = (module_path.to_vec(), name.clone());
        match self.items.spec_state(&key) {
            // `Done` but absent means this spec's own construction already
            // failed (its real diagnostics were recorded then) -- reported as
            // `ItemFailed`, so a second reference doesn't get a misleading
            // "not a spec" instead.
            Some(QueryState::Done) => {
                return match self.items.spec_cell(&key) {
                    Some(cell) => Ok(Some(cell)),
                    None => Err(ResolveError::ItemFailed { module: key.0, item: key.1 }),
                };
            }
            Some(QueryState::InProgress) => {
                return Err(ResolveError::SpecDependencyCycle { module: key.0, spec: key.1 });
            }
            None => {}
        }

        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let HirItem::Spec(sp) = &self.modules.parsed(module_path).hir.items[index] else {
            return Ok(None);
        };
        let sp = sp.clone();

        self.items.begin_spec(&key);
        // Empty substitution: `resolve_spec_dependencies` only identifies
        // *which* spec each dependency names (its args stay raw) and
        // `resolve_spec_functions` is fully deferred, so neither needs `Self`
        // or this spec's own generics bound to anything concrete yet.
        let run = self.with_analyzer(module_path, &[], (sp.id, sp.span), |analyzer| {
            (analyzer.resolve_spec_dependencies(&sp), analyzer.resolve_spec_functions(&sp))
        });
        self.diagnostics.record_warnings(module_path, run.warnings);

        if run.failed {
            self.items.finish_spec(&key, None);
            return Err(ResolveError::ItemFailed { module: key.0, item: key.1 });
        }
        let (dependencies, functions) = run.result;
        let cell = Rc::new(RefCell::new(ResolvedSpecType {
            id: sp.id,
            name: sp.name.clone(),
            visibility: sp.visibility,
            generics: sp.generics.iter().map(|g| g.ident.clone()).collect(),
            module_path: module_path.to_vec(),
            type_args: vec![],
            dependencies,
            functions,
        }));
        self.items.finish_spec(&key, Some(&cell));
        Ok(Some(cell))
    }
}
