//! Phase 2: checking one item's *body*, reading its already-resolved
//! signature back rather than ever re-deriving it.
//!
//! Every entry point here is shared by both of the two ways a body is
//! reached: `compile`'s static per-module sweep (never generic) and the
//! on-demand trigger a fresh generic instantiation fires (a real
//! substitution).

use crate::Driver;
use crate::items::{CheckedBody, ItemKey};
use omega_analyzer::analysis::{Analyzer, item_id_span};
use omega_analyzer::checked::{
    CheckedDeclaration, CheckedEnumDef, CheckedExternDeclaration, CheckedItem, CheckedStructDef,
    CheckedUnionDef,
};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_analyzer::resolver::{ResolveError, ResolvedItem};
use omega_diagnostics::Span;
use omega_hir::{HirGenericParam, HirId, HirItem};
use omega_parser::prelude::Ident;

/// The three checked aggregate shapes differ only in which `CheckedItem`
/// they become; the two things phase 2 does to all of them -- appending the
/// spec-default methods queued in phase 1, and stamping the instantiation's
/// own type arguments -- are identical.
trait CheckedAggregate: Sized {
    fn assemble(self, type_args: Vec<ResolvedType>) -> CheckedItem;
}

impl CheckedAggregate for CheckedStructDef {
    fn assemble(mut self, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.type_args = type_args;
        CheckedItem::Struct(self)
    }
}

impl CheckedAggregate for CheckedEnumDef {
    fn assemble(mut self, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.type_args = type_args;
        CheckedItem::Enum(self)
    }
}

impl CheckedAggregate for CheckedUnionDef {
    fn assemble(mut self, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.type_args = type_args;
        CheckedItem::Union(self)
    }
}

impl Driver {
    /// [`Self::check_item_body`], memoized by [`ItemKey`] -- the one entry
    /// point both `compile`'s own per-module phase-2 sweep and a `comp`
    /// evaluation's on-demand `resolve_function_body` now go through, so
    /// whichever of the two reaches a given item *second* gets the cached
    /// result instead of silently re-checking (and, downstream, re-
    /// lowering/re-codegening) the same body a second time. `index` is the
    /// item's position in its module's own list, needed only on a cache
    /// miss, matching `check_generic_instantiation_body`'s identical shape.
    ///
    /// A generic instantiation reuses `generic_instantiations` (the cache
    /// `ensure_item` itself already populates via `check_generic_
    /// instantiation_body` the moment the instantiation's own signature
    /// resolves) rather than `checked_bodies` -- so a `comp` call that
    /// reaches an instantiation *before* `ensure_item` naturally would
    /// still only computes it once, and `compile`'s own final-assembly
    /// merge (which reads `generic_instantiations` specifically) still
    /// finds it there either way.
    ///
    /// `None` on a self-referential cycle (`items.body_in_progress`) --
    /// possible now in a way it never was before `comp`: body-checking can
    /// reenter itself when a function's own body contains a `comp`
    /// expression that (directly, or through another function) calls back
    /// into the very item currently being checked.
    pub(crate) fn ensure_item_body(&mut self, key: &ItemKey, index: usize) -> Option<CheckedBody> {
        if key.is_instantiation() {
            if let Some(body) = self.items.generic_instantiations.get(key) {
                return Some(body.clone_of());
            }
            if !self.items.body_in_progress.insert(key.clone()) {
                return None;
            }
            self.check_generic_instantiation_body(key, index);
            self.items.body_in_progress.remove(key);
            return self
                .items
                .generic_instantiations
                .get(key)
                .map(CheckedBody::clone_of);
        }

        if let Some(body) = self.items.checked_bodies.get(key) {
            return Some(body.clone_of());
        }
        if !self.items.body_in_progress.insert(key.clone()) {
            return None;
        }
        let hir = self.modules.hir(&key.module);
        let body = self.check_item_body(key, &hir.items[index]);
        self.items.body_in_progress.remove(key);
        let body = body?;
        self.items
            .checked_bodies
            .insert(key.clone(), body.clone_of());
        Some(body)
    }

    /// Checks one item's body, reading its already-`Done` signature straight
    /// out of the query caches. `Declaration`/`ExternDeclaration` have no
    /// body at all, so they need no `Analyzer` -- just their resolved type
    /// paired with the identity already on the HIR node.
    ///
    /// Not memoized on its own -- see [`Self::ensure_item_body`], the entry
    /// point every caller other than `check_generic_instantiation_body`
    /// (whose own `generic_instantiations` cache already serves the same
    /// purpose for an instantiation) should use instead.
    pub(crate) fn check_item_body(&mut self, key: &ItemKey, item: &HirItem) -> Option<CheckedBody> {
        match item {
            HirItem::Declaration(decl) => {
                let r#type = self.resolved_value_type(key);
                let checked = CheckedDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                    mutable: decl.mutable,
                    initial_value: None,
                };
                Some(CheckedBody {
                    item: CheckedItem::Declaration(checked),
                    warnings: vec![],
                })
            }

            // `ident : Type = value;` -- identical shape to the non-`comp`
            // `Walrus` arm below (`initial_value` read back from the same
            // `items.global_initial_values` cache, populated by
            // `compute_item`'s `analyze_global_declaration_with_init`
            // call), just sourced from a `HirDeclaration` instead of a
            // `HirWalrusDeclaration`.
            HirItem::DeclarationWithInit(decl, _) => {
                let r#type = self.resolved_value_type(key);
                let initial_value = self.items.global_initial_values.get(&decl.id).cloned();
                let checked = CheckedDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                    mutable: decl.mutable,
                    initial_value,
                };
                Some(CheckedBody {
                    item: CheckedItem::Declaration(checked),
                    warnings: vec![],
                })
            }

            // A `comp` top-level binding (`w.comp == true`) has no body-
            // phase work left at all -- `compute_item`'s own `Walrus` arm
            // already evaluated it (eagerly, during signature resolution --
            // see that arm's doc comment) and recorded its value in
            // `items.comp_values`. `None`, not a `CheckedBody`: it
            // contributes nothing to the final `CheckedModule` -- every
            // reference substitutes its value directly (`Storage::Comp`),
            // so MIR/codegen never need to see it as an item at all.
            //
            // A non-`comp` `Walrus` (`w.comp == false`) is the opposite:
            // it *does* need to reach MIR/codegen, as a real
            // `Storage::Global` place -- same shape as `HirItem::
            // Declaration` above, just with `initial_value` read back from
            // `items.global_initial_values` (populated by `compute_item`'s
            // own `analyze_global_walrus` call; `None` there simply means
            // this global has no initializer at all, e.g. `pqr : Thing;`
            // spelled with `:=`... which the grammar doesn't actually
            // allow, so in practice this is always `Some` here -- see
            // `CheckedDeclaration::initial_value`'s doc comment).
            HirItem::Walrus(w) if w.comp => None,
            HirItem::Walrus(w) => {
                let r#type = self.resolved_value_type(key);
                let initial_value = self.items.global_initial_values.get(&w.id).cloned();
                let checked = CheckedDeclaration {
                    id: w.id,
                    span: w.span,
                    ident: w.ident.clone(),
                    r#type,
                    mutable: w.mutable,
                    initial_value,
                };
                Some(CheckedBody {
                    item: CheckedItem::Declaration(checked),
                    warnings: vec![],
                })
            }

            HirItem::ExternDeclaration(decl) => {
                let r#type = self.resolved_value_type(key);
                let checked = CheckedExternDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                    mangling: omega_analyzer::annotations::ManglingMode::Disabled,
                };
                Some(CheckedBody {
                    item: CheckedItem::ExternDeclaration(checked),
                    warnings: vec![],
                })
            }

            HirItem::FunctionDefinition(f) => {
                let ResolvedItem::Value {
                    r#type: ResolvedType::Function(fn_type),
                    decl_id,
                    ..
                } = self.items.expect_resolved(key).clone()
                else {
                    unreachable!("a function's own resolved item is always ResolvedType::Function");
                };
                let substitution = Self::substitution(&f.generics, &key.type_args);
                let bounds = self
                    .items
                    .generic_bounds
                    .get(key)
                    .cloned()
                    .unwrap_or_default();
                let annotations = self
                    .items
                    .function_annotations
                    .get(&decl_id)
                    .cloned()
                    .unwrap_or_default();
                let run = self.with_analyzer_in(
                    &key.module,
                    &substitution,
                    &bounds,
                    (f.id, f.span),
                    |analyzer| analyzer.check_function_body(f, &fn_type, decl_id, &annotations),
                );
                run.result.map(|mut checked| {
                    checked.type_args = key.type_args.clone();
                    CheckedBody {
                        item: CheckedItem::FunctionDefinition(checked),
                        warnings: run.warnings,
                    }
                })
            }

            HirItem::Struct(s) => {
                let cell = self.items.cells.expect_struct(key);
                let self_type = ResolvedType::Struct(cell.clone());
                self.check_aggregate_body(key, (s.id, s.span), &s.generics, self_type, |a| {
                    a.check_struct_body(s, &cell)
                })
            }

            HirItem::Enum(e) => {
                let cell = self.items.cells.expect_enum(key);
                let self_type = ResolvedType::Enum {
                    cell: cell.clone(),
                    variant: None,
                };
                self.check_aggregate_body(key, (e.id, e.span), &e.generics, self_type, |a| {
                    a.check_enum_body(e, &cell)
                })
            }

            HirItem::Union(u) => {
                let cell = self.items.cells.expect_union(key);
                let self_type = ResolvedType::Union(cell.clone());
                self.check_aggregate_body(key, (u.id, u.span), &u.generics, self_type, |a| {
                    a.check_union_body(u, &cell)
                })
            }

            // A spec declares no code of its own -- its functions only ever
            // become real bodies through an implementor (or a `for` target).
            HirItem::Spec(_) => None,
            HirItem::Gap(_) | HirItem::Glue(_) | HirItem::Compose(_) | HirItem::Primitive(_) => {
                None
            }
            HirItem::Import(_) => unreachable!("imports are filtered out before this is called"),
        }
    }

    /// The shared spine of `check_item_body`'s struct/enum/union arms: bind
    /// `Self` to the (already resolved) cell, check the bodies, then append
    /// whatever spec-default methods phase 1 queued for this implementor.
    fn check_aggregate_body<C: CheckedAggregate>(
        &mut self,
        key: &ItemKey,
        owner: (HirId, Span),
        generics: &[HirGenericParam],
        self_type: ResolvedType,
        check: impl FnOnce(&mut Analyzer) -> Option<C>,
    ) -> Option<CheckedBody> {
        let mut substitution = Self::substitution(generics, &key.type_args);
        substitution.push((Ident("Self".to_string()), self_type.clone()));

        // An aggregate's own generic bounds only. A type's *inherent*
        // methods are not a compose body, so nothing composed onto this
        // type belongs in their scope -- see `check_compose_bodies`, which
        // seeds a compose body with the one spec it composes, and
        // `check_generic_bounds`, which seeds exactly the declared bound.
        let bounds = self
            .items
            .generic_bounds
            .get(key)
            .cloned()
            .unwrap_or_default();
        let run = self.with_analyzer_in(&key.module, &substitution, &bounds, owner, check);
        run.result.map(|checked| CheckedBody {
            item: checked.assemble(key.type_args.clone()),
            warnings: run.warnings,
        })
    }

    /// Body-checks a *specific* generic instantiation the moment its own
    /// signature finishes (triggered from `ensure_item`). Identical to the
    /// ordinary per-module sweep except for *when* it runs (on demand, since
    /// `compile` cannot enumerate instantiations up front) and *where the
    /// result goes* (merged into its module during final assembly).
    pub(crate) fn check_generic_instantiation_body(&mut self, key: &ItemKey, index: usize) {
        let hir = self.modules.hir(&key.module);
        if let Some(body) = self.check_item_body(key, &hir.items[index]) {
            self.items.generic_instantiations.insert(key.clone(), body);
        }
    }

    /// The resolved type of a bodyless value item (a global or an extern
    /// declaration), which is always a `ResolvedItem::Value`.
    fn resolved_value_type(&self, key: &ItemKey) -> ResolvedType {
        match self.items.expect_resolved(key) {
            ResolvedItem::Value { r#type, .. } => r#type.clone(),
            ResolvedItem::Type(_) | ResolvedItem::Gap(_) => {
                unreachable!("a declaration's own resolved item is always a value")
            }
        }
    }

    /// A generic item's declared parameters zipped with the concrete
    /// arguments this instantiation supplied -- empty for an ordinary item.
    fn substitution(
        generics: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Vec<(Ident, ResolvedType)> {
        generics
            .iter()
            .map(|g| g.ident.clone())
            .zip(type_args.iter().cloned())
            .collect()
    }
}

/// Overload candidates need their own signature/body caches, keyed by
/// position rather than by name: an `ItemKey` can only ever address one item
/// per name, so it would silently only ever reach the first-declared
/// candidate. Every candidate is confirmed a plain, non-generic function when
/// the module is indexed, so there's no instantiation identity to decide here
/// the way `compute_item` has.
impl Driver {
    pub(crate) fn ensure_overload_signature(
        &mut self,
        module_path: &[Ident],
        index: usize,
    ) -> Result<ResolvedFunctionType, ResolveError> {
        let key = (module_path.to_vec(), index);
        if let Some(fn_type) = self.items.overload_signatures.get(&key) {
            return Ok(fn_type.clone());
        }
        let hir = self.modules.hir(module_path);
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index confirmed to be a function");
        };

        // An overloaded free function doesn't yet support `spec T` return-
        // type body inference either -- see the identical scope note on
        // `collect_methods`'s own call site.
        let checked = self.analyze(module_path, &[], (f.id, f.span), |a| {
            a.collect_function_signature(f, None)
        });
        let (fn_type, annotations) = checked.ok_or_else(|| ResolveError::ItemFailed {
            module: module_path.to_vec(),
            item: f.name.clone(),
        })?;

        self.items.function_annotations.insert(f.id, annotations);
        self.items.overload_signatures.insert(key, fn_type.clone());
        Ok(fn_type)
    }

    /// One overload candidate's checked body, reading its own already-
    /// resolved signature back rather than recomputing it.
    pub(crate) fn ensure_overload_body(
        &mut self,
        module_path: &[Ident],
        index: usize,
    ) -> Option<CheckedBody> {
        let key = (module_path.to_vec(), index);
        if let Some(body) = self.items.overload_bodies.get(&key) {
            return Some(body.clone_of());
        }
        let fn_type = self.ensure_overload_signature(module_path, index).ok()?;
        let hir = self.modules.hir(module_path);
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index confirmed to be a function");
        };
        let annotations = self
            .items
            .function_annotations
            .get(&f.id)
            .cloned()
            .unwrap_or_default();

        let run = self.with_analyzer(module_path, &[], (f.id, f.span), |analyzer| {
            analyzer.check_function_body(f, &fn_type, f.id, &annotations)
        });
        let body = CheckedBody {
            item: CheckedItem::FunctionDefinition(run.result?),
            warnings: run.warnings,
        };
        self.items.overload_bodies.insert(key, body.clone_of());
        Some(body)
    }

    /// Compares every pair of `name`'s overload candidates by param-type list,
    /// ignoring parameter names -- an identical pair is a genuine duplicate
    /// (no call could ever tell them apart), reported through the same
    /// `Redeclaration` diagnostic a same-shaped non-function collision gets,
    /// since the underlying meaning ("this name already exists here") is
    /// identical.
    pub(crate) fn check_overload_duplicates(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        indices: &[usize],
        signatures: &[ResolvedFunctionType],
    ) {
        let hir = self.modules.hir(module_path);
        let same_params = |a: &ResolvedFunctionType, b: &ResolvedFunctionType| {
            a.params
                .iter()
                .map(|(_, t)| t)
                .eq(b.params.iter().map(|(_, t)| t))
        };
        for i in 1..indices.len() {
            let Some(j) = (0..i).find(|&j| same_params(&signatures[i], &signatures[j])) else {
                continue;
            };
            let (id, span) = item_id_span(&hir.items[indices[i]]);
            let (_, previous) = item_id_span(&hir.items[indices[j]]);
            self.diagnostics.error(
                module_path,
                AnalysisError::new(
                    id,
                    span,
                    AnalysisErrorKind::Redeclaration {
                        name: name.clone(),
                        previous: Some(previous),
                    },
                ),
            );
        }
    }
}
