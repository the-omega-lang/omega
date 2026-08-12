//! The one global, item-granular query behind same-module resolution,
//! cross-module resolution, and generic instantiation alike.
//!
//! Every top-level item is its own independent, memoized query, so one bad
//! item never poisons an unrelated sibling's, and declaration order never
//! matters anywhere.

use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::analysis::{Analyzer, item_id_span, item_visibility};
use omega_analyzer::annotations::ResolvedAnnotations;
use omega_analyzer::checked::{CheckedItem, Storage};
use omega_analyzer::error::AnalysisWarning;
use omega_analyzer::resolved_type::{
    ResolvedBound, ResolvedEnumType, ResolvedFunctionType, ResolvedMethod, ResolvedSpecType,
    ResolvedStructType, ResolvedType, ResolvedUnionType,
};
use omega_analyzer::resolver::{ResolveError, ResolvedItem};
use omega_diagnostics::Span;
use omega_hir::{HirFunctionDef, HirGenericParam, HirId, HirItem, SYNTHETIC_MODULE};
use omega_parser::prelude::{Ident, Type, Visibility};
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
        Self {
            module: module.to_vec(),
            name: name.clone(),
            type_args: type_args.to_vec(),
        }
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
        ResolveError::ItemFailed {
            module: self.module.clone(),
            item: self.name.clone(),
        }
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

pub(crate) struct GlueSignature {
    pub module: ModulePath,
    pub id: HirId,
    pub gap: Rc<omega_analyzer::resolved_type::ResolvedGap>,
    pub functions: Vec<(HirFunctionDef, ResolvedFunctionType)>,
}

impl CheckedBody {
    pub fn clone_of(&self) -> Self {
        Self {
            item: self.item.clone(),
            warnings: self.warnings.clone(),
        }
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
                    is_marker: false,
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
        self.structs
            .get(key)
            .expect("cell was created when the signature resolved")
            .clone()
    }

    pub fn expect_enum(&self, key: &ItemKey) -> Rc<RefCell<ResolvedEnumType>> {
        self.enums
            .get(key)
            .expect("cell was created when the signature resolved")
            .clone()
    }

    pub fn expect_union(&self, key: &ItemKey) -> Rc<RefCell<ResolvedUnionType>> {
        self.unions
            .get(key)
            .expect("cell was created when the signature resolved")
            .clone()
    }

    /// `key`'s existing cell as a type, whichever of the three kinds it is --
    /// `None` when `key` has no cell at all (i.e. it isn't an aggregate).
    pub fn resolved_type(&self, key: &ItemKey) -> Option<ResolvedType> {
        if let Some(cell) = self.structs.get(key) {
            return Some(ResolvedType::Struct(cell.clone()));
        }
        if let Some(cell) = self.enums.get(key) {
            return Some(ResolvedType::Enum {
                cell: cell.clone(),
                variant: None,
            });
        }
        self.unions
            .get(key)
            .map(|cell| ResolvedType::Union(cell.clone()))
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
            .chain(
                self.enums
                    .iter()
                    .map(|(key, cell)| (key, cell.borrow().functions.clone())),
            )
            .chain(
                self.unions
                    .iter()
                    .map(|(key, cell)| (key, cell.borrow().functions.clone())),
            )
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
    pub gaps: HashMap<ItemKey, Rc<omega_analyzer::resolved_type::ResolvedGap>>,
    pub glues: Vec<GlueSignature>,
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
    /// The concrete compose contexts established by this instantiation's
    /// generic bounds. Body analysis consults only these entries for
    /// instance-method syntax supplied by a composition.
    pub generic_bounds: HashMap<ItemKey, Vec<ResolvedBound>>,
    /// Counter behind every synthetic `HirId`. Always paired with
    /// `SYNTHETIC_MODULE`, a module id the lowerer never produces, so these
    /// can never collide with a real per-file id.
    next_synthetic_id: u32,
    /// Every non-generic, non-overload item's checked body, memoized the
    /// first time anything asks for it -- either `compile`'s own per-module
    /// phase-2 sweep (`check_module_bodies`), or a `comp` evaluation
    /// reaching it early, out of that sweep's own order, via
    /// `resolve_function_body`. Without this, whichever of the two asks
    /// second would re-check (and, for a function, re-lower/re-codegen) the
    /// exact same body, producing a duplicate-symbol link error -- the same
    /// failure mode `overload_bodies`/`generic_instantiations` already exist
    /// to avoid for their own cases. `ensure_item_body` (`bodies.rs`) is the
    /// one entry point both callers now go through.
    pub checked_bodies: HashMap<ItemKey, CheckedBody>,
    /// The reverse of an ordinary forward `ItemKey` lookup: given a
    /// function/method's own already-decided identity (`identity_for`'s
    /// return value), which item it belongs to. Exists purely for `comp`
    /// evaluation (`resolve_function_body`), which is handed a bare
    /// `HirId` -- the only identity a `CheckedPlaceRoot::Variable`'s
    /// `Storage::Function` root carries -- and needs to work backwards to
    /// "which `ItemKey` do I `ensure_item_body` to get this body checked".
    /// A method's id maps to its *owner* aggregate's own key (methods have
    /// no `ItemKey` of their own; their bodies are only ever checked as
    /// part of their owner's), matching `identity_for`'s own call sites
    /// (`method_identities` calls it once per method, but always with the
    /// owner's `key`).
    pub decl_id_owner: HashMap<HirId, ItemKey>,
    /// Every top-level `comp` binding's already-evaluated value
    /// (`HirItem::Walrus`, always `comp` -- see `Analyzer::
    /// analyze_comp_declaration`), keyed by its own `decl_id`. The driver-
    /// level counterpart of `omega_analyzer::context::Context::
    /// comp_values`, which only ever holds *local* bindings (scoped to one
    /// throwaway per-item `Analyzer`) -- a global's value has to survive
    /// past that, for every other item's own separate analysis to read
    /// back via `ModuleResolver::resolve_comp_value`.
    pub comp_values: HashMap<HirId, omega_analyzer::resolved_type::ConstValue>,
    /// The identical cross-phase-survival need as `comp_values`, for a
    /// non-`comp` top-level binding's compile-time-known initial value
    /// (`HirItem::Walrus`, `w.comp == false`, with a value) -- `compute_item`
    /// (signature resolution) is the only phase that actually runs
    /// `Analyzer::analyze_global_walrus`; `check_item_body` (a separate,
    /// possibly much later call) reads the result back from here to build
    /// this global's `CheckedDeclaration` rather than re-analyzing the
    /// initializer a second time. Unlike `comp_values`, absence here is
    /// meaningful too (a `mut pqr : Thing;`-shaped global with no
    /// initializer at all): `check_item_body` treats a missing entry as
    /// `initial_value: None`, not an error.
    pub global_initial_values: HashMap<HirId, omega_analyzer::resolved_type::ConstValue>,
    /// The body-checking counterpart of `state`'s `InProgress` guard --
    /// needed because body-checking (unlike signature resolution) can now
    /// genuinely reenter itself: a `comp` expression inside function `f`'s
    /// own body can call `resolve_function_body` on `f` itself (directly,
    /// or through a `g` that calls back into `f`) before `f`'s own body has
    /// finished checking, since `comp` evaluation runs *during* body-
    /// checking rather than only after it. `ensure_item_body` reports this
    /// as a failure (rather than reentering, which would recurse the real
    /// Rust call stack without bound -- the interpreter's own fuel/depth
    /// budget bounds *interpretation*, not this, a level below it) the
    /// moment it finds `key` already present here.
    pub body_in_progress: std::collections::HashSet<ItemKey>,
    /// Keys currently having their own `spec T` (static-dispatch) return
    /// type inferred from their body (see `Driver::resolve_spec_return_function`).
    /// A plain top-level function's signature is ordinarily fully resolved
    /// before any body anywhere is ever checked (`compile`'s whole-program
    /// phase barrier) -- a `spec T`-returning function's signature can't be,
    /// since discovering its own concrete return type means checking its
    /// body *during* phase 1, out of the normal order. That inversion is
    /// exactly what could recurse forever for two functions whose inference
    /// calls each other (neither key is ever `InProgress` for *itself* the
    /// way an ordinary same-key cycle would be caught by `state` above) --
    /// this stack is a second, narrower cycle guard for that one case,
    /// mirroring `spec_state`'s identical reasoning for why spec-declaration
    /// resolution needs its own guard instead of relying on `state`.
    spec_return_inference_stack: Vec<ItemKey>,
}

impl ItemQueries {
    /// A fresh, globally unique `HirId` for something with no HIR node of its
    /// own: a generic instantiation's identity, or a spec-default method
    /// instantiated for a concrete implementor.
    pub fn fresh_synthetic_id(&mut self) -> HirId {
        let id = HirId {
            module: SYNTHETIC_MODULE,
            local: self.next_synthetic_id,
        };
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
        let id = if key.is_instantiation() {
            self.fresh_synthetic_id()
        } else {
            declared
        };
        self.decl_id_owner.insert(id, key.clone());
        id
    }

    /// [`Self::identity_for`] over an item's whole method list.
    pub fn method_identities(
        &mut self,
        key: &ItemKey,
        declared: impl IntoIterator<Item = HirId>,
    ) -> Vec<HirId> {
        declared
            .into_iter()
            .map(|id| self.identity_for(key, id))
            .collect()
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
            self.resolved.insert(
                key.clone(),
                ResolvedEntry {
                    visibility,
                    item: item.clone(),
                },
            );
        }
    }

    /// A finished query's outcome -- `None` when it finished by failing.
    pub fn finished(&self, key: &ItemKey) -> Option<(Visibility, ResolvedItem)> {
        self.resolved
            .get(key)
            .map(|entry| (entry.visibility, entry.item.clone()))
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
        &self
            .resolved
            .get(key)
            .expect("every signature is resolved before its body is checked")
            .item
    }

    /// Every resolved item's key paired with its signature -- the whole-cache
    /// walk `collect_extern_functions` needs.
    pub fn resolved_items(&self) -> impl Iterator<Item = (&ItemKey, &ResolvedItem)> {
        self.resolved.iter().map(|(key, entry)| (key, &entry.item))
    }
}

impl Driver {
    /// `exposed`/`internal`/(default hidden)'s access decision --
    /// `declaring`/`accessor` are absolute module paths. `Internal` is
    /// package-wide (same root segment), Rust `pub(crate)`-style, rather than
    /// the narrower "declaring module and its descendants only".
    pub(crate) fn visibility_allows(
        visibility: Visibility,
        declaring: &[Ident],
        accessor: &[Ident],
    ) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Internal => declaring.first() == accessor.first(),
            Visibility::Hidden => declaring == accessor,
        }
    }

    /// One item's declared visibility, read straight off its HIR -- the
    /// property of a *declaration*, so it's identical for every instantiation
    /// of a generic template and needs no resolution to answer.
    pub(crate) fn declared_visibility(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Option<Visibility> {
        let index = self.local_item_index(module_path, name).ok()?;
        self.modules
            .parsed(module_path)
            .hir
            .items
            .get(index)
            .map(item_visibility)
    }

    /// Gates a resolved item behind its own declared visibility. `bypass`
    /// (the `reveal` modifier) suppresses only this one check; it never
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
            Err(ResolveError::NotVisible {
                module: key.module.clone(),
                item: key.name.clone(),
            })
        }
    }

    /// What a request for an item that is *already being resolved* gets.
    ///
    /// A pointer reference never needs its pointee's layout, so it can be
    /// served straight from the type's cell. Everything else is a genuine
    /// cycle: a by-value reference closes an infinite-size type, and an
    /// *import* (always indirect, whatever it names) looping back on an
    /// in-progress non-type item is a real mutual item-import cycle.
    fn in_progress_result(
        &self,
        key: &ItemKey,
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
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
        // `index`/`generic_params` (and, when `type_args` is short, the
        // padded-with-defaults list) have to be known *before* `ItemKey` is
        // built: the key's own equality is purely structural over
        // `type_args`, so two call sites that end up meaning "the same
        // effective types" -- one spelling every argument out, one omitting
        // a defaulted trailing one -- must produce the identical key to
        // share one monomorphized instantiation. This runs even on what
        // will turn out to be a cache hit; both lookups are cheap, "no
        // analysis triggered" reads (see their own doc comments).
        let index = self.local_item_index(module_path, name)?;
        let generic_params = self.item_generics(module_path, name)?;
        let type_args =
            self.pad_generic_defaults(module_path, name, index, &generic_params, type_args)?;
        let key = ItemKey::new(module_path, name, &type_args);

        match self.items.state(&key) {
            Some(QueryState::Done) => {
                let Some((visibility, item)) = self.items.finished(&key) else {
                    return Err(key.failed());
                };
                return Self::gate_visibility(item, visibility, &key, accessor_module_path, bypass);
            }
            Some(QueryState::InProgress) => return self.in_progress_result(&key, indirect),
            None => {}
        }

        if generic_params.iter().any(|g| g.bound.is_some()) {
            self.check_generic_bounds(&key, index, &generic_params, &type_args)?;
        }

        let visibility = self
            .declared_visibility(module_path, name)
            .expect("just indexed by local_item_index");
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

    /// Pads a short `type_args` up to `generic_params.len()` by resolving
    /// each missing *trailing* parameter's own declared default, in order --
    /// each default may itself reference an earlier parameter (`struct
    /// Pair<A, B = A>`), so it's resolved under the substitution built from
    /// every parameter already concrete at that point (mirrors
    /// `check_generic_bounds`'s own substitution-building, just grown
    /// incrementally instead of all at once). A too-long `type_args`, or a
    /// missing parameter with no default, is `GenericArgCountMismatch`
    /// exactly as before this feature existed -- the trailing-only rule
    /// (enforced at parse time, see `omega_parser`'s
    /// `DefaultGenericParamNotTrailing`) guarantees there's never a gap
    /// *before* a still-explicit argument to worry about.
    fn pad_generic_defaults(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        index: usize,
        generic_params: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Result<Vec<ResolvedType>, ResolveError> {
        if type_args.len() >= generic_params.len() {
            if type_args.len() > generic_params.len() {
                return Err(ResolveError::GenericArgCountMismatch {
                    module: module_path.to_vec(),
                    item: name.clone(),
                    expected: generic_params.len(),
                    found: type_args.len(),
                });
            }
            return Ok(type_args.to_vec());
        }

        let hir = self.modules.hir(module_path);
        let owner = item_id_span(&hir.items[index]);
        let mut padded = type_args.to_vec();
        for param in &generic_params[type_args.len()..] {
            let Some(default) = &param.default else {
                return Err(ResolveError::GenericArgCountMismatch {
                    module: module_path.to_vec(),
                    item: name.clone(),
                    expected: generic_params.len(),
                    found: type_args.len(),
                });
            };
            let substitution: Vec<(Ident, ResolvedType)> = generic_params
                .iter()
                .map(|g| g.ident.clone())
                .zip(padded.iter().cloned())
                .collect();
            let default = default.clone();
            let run = self.with_analyzer(module_path, &[], owner, |analyzer| {
                analyzer.resolve_under_substitution(owner.0, owner.1, &default, &substitution)
            });
            match (run.failed, run.result) {
                (false, Some(resolved)) => padded.push(resolved),
                _ => {
                    return Err(ResolveError::ItemFailed {
                        module: module_path.to_vec(),
                        item: name.clone(),
                    });
                }
            }
        }
        Ok(padded)
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
        let substitution: Vec<(Ident, ResolvedType)> = generic_params
            .iter()
            .map(|g| g.ident.clone())
            .zip(type_args.iter().cloned())
            .collect();
        let mut resolved_bounds = Vec::new();

        for (param, concrete) in generic_params.iter().zip(type_args) {
            let Some(bound) = param.bound.clone() else {
                continue;
            };
            let run = self.with_analyzer(&key.module, &substitution, owner, |analyzer| {
                analyzer.check_generic_bound(owner.0, owner.1, &bound, concrete)
            });
            if run.failed {
                return Err(key.failed());
            }
            match run.result {
                Some(Ok((spec, spec_args))) => {
                    resolved_bounds.push((concrete.clone(), spec, spec_args));
                }
                Some(Err((spec, missing))) => {
                    return Err(ResolveError::SpecNotImplemented {
                        type_name: concrete.to_string(),
                        spec,
                        missing,
                    });
                }
                None => {}
            }
        }
        self.items
            .generic_bounds
            .insert(key.clone(), resolved_bounds);
        Ok(())
    }

    /// Resolves one item's signature -- the work `ensure_item` defers to the
    /// first time a name is requested. Each kind gets its own throwaway
    /// `Analyzer`, seeded with the instantiation's substitution.
    fn compute_item(
        &mut self,
        key: &ItemKey,
        index: usize,
        generics: &[Ident],
    ) -> Result<ResolvedItem, ResolveError> {
        let hir = self.modules.hir(&key.module);
        let item = &hir.items[index];
        let module = &key.module;
        let substitution: Vec<(Ident, ResolvedType)> = generics
            .iter()
            .cloned()
            .zip(key.type_args.iter().cloned())
            .collect();

        let resolved = match item {
            HirItem::Declaration(decl) => self
                .analyze(module, &substitution, (decl.id, decl.span), |a| {
                    a.analyze_declaration(decl, Storage::Global)
                })
                .map(|c| ResolvedItem::Value {
                    r#type: c.r#type,
                    storage: Storage::Global,
                    decl_id: c.id,
                    mutable: c.mutable,
                }),

            // `ident : Type = value;` -- `HirItem::Declaration`'s explicit-
            // initializer sibling, same "must be compile-time-known" rule
            // as a non-`comp` `Walrus` below, just with a written-down
            // type instead of an inferred one. See `Analyzer::
            // analyze_global_declaration_with_init`'s own doc comment.
            HirItem::DeclarationWithInit(decl, value) => self
                .analyze(module, &substitution, (decl.id, decl.span), |a| {
                    a.analyze_global_declaration_with_init(decl, value)
                })
                .map(|c| {
                    if let Some(v) = c.initial_value {
                        self.items.global_initial_values.insert(c.id, v);
                    }
                    ResolvedItem::Value {
                        r#type: c.r#type,
                        storage: Storage::Global,
                        decl_id: c.id,
                        mutable: c.mutable,
                    }
                }),

            // A top-level binding, `comp` or not -- evaluated right here,
            // during signature resolution (not deferred to a body-check
            // phase the way a function's own body would be): `comp <expr>`
            // interprets eagerly as an inherent part of ordinary expression
            // analysis (`Analyzer::analyze_comp`), so analyzing `w.value`
            // at all already triggers it, the same "signature resolution
            // that needs a body-shaped answer" inversion `resolve_spec_
            // return_function` uses for the identical reason -- safe here
            // for the same reason it's safe there (`ensure_item_body`'s own
            // cycle guard, see its doc comment, protects the one new
            // reentrancy hazard this opens). `check_item_body`'s own
            // `Walrus` arm has nothing left to do -- see its doc comment.
            //
            // `w.comp` decides which of two genuinely different things
            // this binding is (see `CheckedDeclaration::initial_value`'s
            // doc comment): a `comp` binding has no storage at all, so its
            // value lives only in `ItemQueries::comp_values`, substituted
            // at every use; a non-`comp` binding gets real `Storage::
            // Global` storage (optionally starting from a compile-time-
            // known value), the same as `HirItem::Declaration` above --
            // `analyze_global_walrus` builds the identical `CheckedDeclaration`
            // shape `analyze_declaration` does, just with `initial_value: Some`.
            HirItem::Walrus(w) if w.comp => self
                .analyze(module, &substitution, (w.id, w.span), |a| {
                    a.analyze_comp_declaration(w)
                })
                .map(|(r#type, value)| {
                    self.items.comp_values.insert(w.id, value);
                    ResolvedItem::Value {
                        r#type,
                        storage: Storage::Comp,
                        decl_id: w.id,
                        mutable: false,
                    }
                }),
            HirItem::Walrus(w) => self
                .analyze(module, &substitution, (w.id, w.span), |a| {
                    a.analyze_global_walrus(w)
                })
                .map(|c| {
                    if let Some(value) = c.initial_value {
                        self.items.global_initial_values.insert(c.id, value);
                    }
                    ResolvedItem::Value {
                        r#type: c.r#type,
                        storage: Storage::Global,
                        decl_id: c.id,
                        mutable: c.mutable,
                    }
                }),

            HirItem::ExternDeclaration(decl) => self
                .analyze(module, &substitution, (decl.id, decl.span), |a| {
                    a.analyze_extern_decl(decl)
                })
                .map(|c| {
                    let storage = match c.r#type {
                        ResolvedType::Function(_) => Storage::Function,
                        _ => Storage::Global,
                    };
                    // `extern` declarations are always immutable for now --
                    // see `Analyzer::analyze_extern_decl`'s own doc comment.
                    ResolvedItem::Value {
                        r#type: c.r#type,
                        storage,
                        decl_id: c.id,
                        mutable: false,
                    }
                }),

            HirItem::FunctionDefinition(f) => {
                let id = self.items.identity_for(key, f.id);
                if let Type::SpecStatic(bound) = &f.return_type {
                    self.resolve_spec_return_function(key, f, id, bound, module, &substitution)?
                } else {
                    self.analyze(module, &substitution, (f.id, f.span), |a| {
                        a.collect_function_signature(f, None)
                    })
                    .map(|(fn_type, annotations)| {
                        self.items.function_annotations.insert(id, annotations);
                        ResolvedItem::Value {
                            r#type: ResolvedType::Function(fn_type),
                            storage: Storage::Function,
                            decl_id: id,
                            mutable: false,
                        }
                    })
                }
            }

            HirItem::Struct(s) => {
                let id = self.items.identity_for(key, s.id);
                let cell = self.items.cells.struct_cell(key, id);
                let method_ids = self
                    .items
                    .method_identities(key, s.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Struct(cell.clone());
                self.compute_aggregate(
                    key,
                    (s.id, s.span),
                    &substitution,
                    self_type,
                    method_ids,
                    |a, ids| a.signature_of_struct(s, &cell, ids),
                )
            }

            HirItem::Enum(e) => {
                let id = self.items.identity_for(key, e.id);
                let cell = self.items.cells.enum_cell(key, id);
                let method_ids = self
                    .items
                    .method_identities(key, e.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Enum {
                    cell: cell.clone(),
                    variant: None,
                };
                self.compute_aggregate(
                    key,
                    (e.id, e.span),
                    &substitution,
                    self_type,
                    method_ids,
                    |a, ids| a.signature_of_enum(e, &cell, ids),
                )
            }

            HirItem::Union(u) => {
                let id = self.items.identity_for(key, u.id);
                let cell = self.items.cells.union_cell(key, id);
                let method_ids = self
                    .items
                    .method_identities(key, u.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Union(cell.clone());
                self.compute_aggregate(
                    key,
                    (u.id, u.span),
                    &substitution,
                    self_type,
                    method_ids,
                    |a, ids| a.signature_of_union(u, &cell, ids),
                )
            }

            HirItem::Gap(gap) => {
                let id = self.items.identity_for(key, gap.id);
                self.analyze(module, &substitution, (gap.id, gap.span), |a| {
                    a.signature_of_gap(gap)
                })
                .map(|mut gap| {
                    gap.id = id;
                    let gap = Rc::new(gap);
                    self.items.gaps.insert(key.clone(), gap.clone());
                    ResolvedItem::Gap(gap)
                })
            }

            // A spec's cell is genuinely args-independent, so its
            // construction is fully delegated (and its own diagnostics
            // recorded) inside `resolve_spec_declaration`. This arm exists
            // only to serve the ordinary, already-arg-count-validated
            // reference path.
            HirItem::Spec(_) => {
                let absolute: Vec<Ident> =
                    module.iter().cloned().chain([key.name.clone()]).collect();
                self.resolve_spec_declaration(&absolute)?
                    .map(|cell| ResolvedItem::Type(ResolvedType::Spec(cell)))
            }

            HirItem::Glue(_) | HirItem::Compose(_) | HirItem::Primitive(_) => {
                unreachable!("unnamed blocks have no item key")
            }
            HirItem::Import(_) => unreachable!("imports are never indexed into a module's items"),
        };

        resolved.ok_or_else(|| key.failed())
    }

    /// The shared spine of `compute_item`'s struct/enum/union arms: bind
    /// `Self` to the cell and resolve the signature.
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
        signature: impl FnOnce(&mut Analyzer, &[HirId]) -> Option<()>,
    ) -> Option<ResolvedItem> {
        let mut substitution = substitution.to_vec();
        substitution.push((Ident("Self".to_string()), self_type.clone()));

        self.analyze(&key.module, &substitution, owner, |analyzer| {
            signature(analyzer, &method_ids)
        })?;
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
                    None => Err(ResolveError::ItemFailed {
                        module: key.0,
                        item: key.1,
                    }),
                };
            }
            Some(QueryState::InProgress) => {
                return Err(ResolveError::SpecDependencyCycle {
                    module: key.0,
                    spec: key.1,
                });
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
            (
                analyzer.resolve_spec_dependencies(&sp),
                analyzer.resolve_spec_functions(&sp),
            )
        });
        self.diagnostics.record_warnings(module_path, run.warnings);

        if run.failed {
            self.items.finish_spec(&key, None);
            return Err(ResolveError::ItemFailed {
                module: key.0,
                item: key.1,
            });
        }
        let (dependencies, (functions, annotations)) = run.result;
        // See `ResolvedSpecType::is_object_safe`'s doc comment: computed
        // once, here, since `functions`/`dependencies` are both already
        // fully resolved (a dependency's own cell is always `Done` -- and
        // so already carries its own `is_object_safe` -- by the time it's
        // sitting in this list).
        let is_object_safe = functions
            .iter()
            .all(|(_, raw)| !matches!(raw.return_type, Type::SpecStatic(_)))
            && dependencies
                .iter()
                .all(|(dep, _)| dep.borrow().is_object_safe);
        let cell = Rc::new(RefCell::new(ResolvedSpecType {
            id: sp.id,
            name: sp.name.clone(),
            visibility: sp.visibility,
            generics: sp.generics.iter().map(|g| g.ident.clone()).collect(),
            module_path: module_path.to_vec(),
            type_args: vec![],
            is_object_safe,
            dependencies,
            functions,
            suppress: annotations.suppress,
        }));
        self.items.finish_spec(&key, Some(&cell));
        Ok(Some(cell))
    }

    /// A `spec T` (static-dispatch) return-type function's signature can't
    /// be resolved the ordinary way at all -- `f.return_type` names a bound,
    /// not a concrete type, so `collect_function_signature` has nothing to
    /// give `resolve_type_or_error`. This eagerly infers the concrete return
    /// type from the function's own body (`Analyzer::infer_body_return_type`)
    /// *before* the signature can be considered resolved -- a genuine
    /// inversion of the ordinary phase-1-before-phase-2 order, but a
    /// contained one: once the concrete type is known, `collect_function_
    /// signature` runs exactly as it always does (with the inferred type as
    /// `return_type_override`), and the ordinary phase-2 sweep reads the
    /// result back like any other function's -- no separate body cache is
    /// needed, since (unlike a generic instantiation or an overload
    /// candidate) an ordinary top-level function is already fully enumerable
    /// by `compile`'s static per-module sweep.
    ///
    /// `spec_return_inference_stack` guards against exactly the recursion
    /// this inversion opens up: two spec-return functions whose inference
    /// calls each other would otherwise recurse forever, since neither
    /// function's own key is ever `InProgress` (in the ordinary `ensure_item`
    /// sense) for *itself* the way a same-key cycle would normally be caught
    /// -- see `ItemQueries::spec_return_inference_stack`'s doc comment.
    fn resolve_spec_return_function(
        &mut self,
        key: &ItemKey,
        f: &HirFunctionDef,
        id: HirId,
        bound: &Type,
        module: &[Ident],
        substitution: &[(Ident, ResolvedType)],
    ) -> Result<Option<ResolvedItem>, ResolveError> {
        if self.items.spec_return_inference_stack.contains(key) {
            return Err(ResolveError::SpecReturnTypeRecursion {
                module: key.module.clone(),
                item: key.name.clone(),
            });
        }
        self.items.spec_return_inference_stack.push(key.clone());
        let inferred = self.analyze(module, substitution, (f.id, f.span), |a| {
            a.infer_body_return_type(f, bound)
        });
        self.items.spec_return_inference_stack.pop();

        let Some(return_type) = inferred else {
            return Ok(None);
        };

        let checked = self.analyze(module, substitution, (f.id, f.span), |a| {
            a.collect_function_signature(f, Some(return_type))
        });
        Ok(checked.map(|(fn_type, annotations)| {
            self.items.function_annotations.insert(id, annotations);
            ResolvedItem::Value {
                r#type: ResolvedType::Function(fn_type),
                storage: Storage::Function,
                decl_id: id,
                mutable: false,
            }
        }))
    }
}
