//! Phase 2: checking one item's *body*, reading its already-resolved
//! signature back rather than ever re-deriving it.
//!
//! Every entry point here is shared by both of the two ways a body is
//! reached: `compile`'s static per-module sweep (never generic) and the
//! on-demand trigger a fresh generic instantiation fires (a real
//! substitution).

use crate::items::{CheckedBody, ItemKey};
use crate::Driver;
use omega_analyzer::analysis::{Analyzer, item_id_span};
use omega_analyzer::checked::{
    CheckedDeclaration, CheckedEnumDef, CheckedExternDeclaration, CheckedFunctionDef, CheckedItem, CheckedStructDef,
    CheckedUnionDef,
};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind, AnalysisWarning};
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
    fn assemble(self, spec_functions: Vec<CheckedFunctionDef>, type_args: Vec<ResolvedType>) -> CheckedItem;
}

impl CheckedAggregate for CheckedStructDef {
    fn assemble(mut self, spec_functions: Vec<CheckedFunctionDef>, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.functions.extend(spec_functions);
        self.type_args = type_args;
        CheckedItem::Struct(self)
    }
}

impl CheckedAggregate for CheckedEnumDef {
    fn assemble(mut self, spec_functions: Vec<CheckedFunctionDef>, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.functions.extend(spec_functions);
        self.type_args = type_args;
        CheckedItem::Enum(self)
    }
}

impl CheckedAggregate for CheckedUnionDef {
    fn assemble(mut self, spec_functions: Vec<CheckedFunctionDef>, type_args: Vec<ResolvedType>) -> CheckedItem {
        self.functions.extend(spec_functions);
        self.type_args = type_args;
        CheckedItem::Union(self)
    }
}

impl Driver {
    /// Checks one item's body, reading its already-`Done` signature straight
    /// out of the query caches. `Declaration`/`ExternDeclaration` have no
    /// body at all, so they need no `Analyzer` -- just their resolved type
    /// paired with the identity already on the HIR node.
    pub(crate) fn check_item_body(&mut self, key: &ItemKey, item: &HirItem) -> Option<CheckedBody> {
        match item {
            HirItem::Declaration(decl) => {
                let r#type = self.resolved_value_type(key);
                let checked = CheckedDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                };
                Some(CheckedBody { item: CheckedItem::Declaration(checked), warnings: vec![] })
            }

            HirItem::ExternDeclaration(decl) => {
                let r#type = self.resolved_value_type(key);
                let checked = CheckedExternDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                };
                Some(CheckedBody { item: CheckedItem::ExternDeclaration(checked), warnings: vec![] })
            }

            HirItem::FunctionDefinition(f) => {
                let ResolvedItem::Value { r#type: ResolvedType::Function(fn_type), decl_id, .. } =
                    self.items.expect_resolved(key).clone()
                else {
                    unreachable!("a function's own resolved item is always ResolvedType::Function");
                };
                let substitution = Self::substitution(&f.generics, &key.type_args);
                let annotations = self.items.function_annotations.get(&decl_id).cloned().unwrap_or_default();
                let run = self.with_analyzer(&key.module, &substitution, (f.id, f.span), |analyzer| {
                    analyzer.check_function_body(f, &fn_type, decl_id, &annotations)
                });
                run.result.map(|mut checked| {
                    checked.type_args = key.type_args.clone();
                    CheckedBody { item: CheckedItem::FunctionDefinition(checked), warnings: run.warnings }
                })
            }

            HirItem::Struct(s) => {
                let cell = self.items.cells.expect_struct(key);
                let self_type = ResolvedType::Struct(cell.clone());
                self.check_aggregate_body(key, (s.id, s.span), &s.generics, self_type, |a| a.check_struct_body(s, &cell))
            }

            HirItem::Enum(e) => {
                let cell = self.items.cells.expect_enum(key);
                let self_type = ResolvedType::Enum { cell: cell.clone(), variant: None };
                self.check_aggregate_body(key, (e.id, e.span), &e.generics, self_type, |a| a.check_enum_body(e, &cell))
            }

            HirItem::Union(u) => {
                let cell = self.items.cells.expect_union(key);
                let self_type = ResolvedType::Union(cell.clone());
                self.check_aggregate_body(key, (u.id, u.span), &u.generics, self_type, |a| a.check_union_body(u, &cell))
            }

            // A spec declares no code of its own -- its functions only ever
            // become real bodies through an implementor (or a `for` target).
            HirItem::Spec(_) => None,
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
        substitution.push((Ident("Self".to_string()), self_type));

        let run = self.with_analyzer(&key.module, &substitution, owner, check);
        let mut warnings = run.warnings;
        let (spec_functions, spec_warnings) = self.check_pending_spec_methods(key);
        warnings.extend(spec_warnings);

        run.result.map(|checked| CheckedBody {
            item: checked.assemble(spec_functions, key.type_args.clone()),
            warnings,
        })
    }

    /// Checks every spec-default-method instantiation queued for `key` during
    /// phase 1. Each gets its own fresh `Analyzer`, seeded with exactly its
    /// own `Self`/spec-generics substitution -- never the implementor's own
    /// generics, which the spec's HIR cannot reference.
    fn check_pending_spec_methods(&mut self, key: &ItemKey) -> (Vec<CheckedFunctionDef>, Vec<AnalysisWarning>) {
        let pending = self.items.pending_spec_methods.get(key).cloned().unwrap_or_default();
        let mut functions = Vec::with_capacity(pending.len());
        let mut warnings = Vec::new();
        for method in pending {
            let run =
                self.with_analyzer(&key.module, &method.substitution, (method.id, method.raw.span), |a| {
                    a.check_pending_spec_method(&method)
                });
            functions.extend(run.result);
            warnings.extend(run.warnings);
        }
        (functions, warnings)
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
            ResolvedItem::Type(_) => unreachable!("a declaration's own resolved item is always a value"),
        }
    }

    /// A generic item's declared parameters zipped with the concrete
    /// arguments this instantiation supplied -- empty for an ordinary item.
    fn substitution(generics: &[HirGenericParam], type_args: &[ResolvedType]) -> Vec<(Ident, ResolvedType)> {
        generics.iter().map(|g| g.ident.clone()).zip(type_args.iter().cloned()).collect()
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

        let checked = self.analyze(module_path, &[], (f.id, f.span), |a| a.collect_function_signature(f));
        let (fn_type, annotations) =
            checked.ok_or_else(|| ResolveError::ItemFailed { module: module_path.to_vec(), item: f.name.clone() })?;

        self.items.function_annotations.insert(f.id, annotations);
        self.items.overload_signatures.insert(key, fn_type.clone());
        Ok(fn_type)
    }

    /// One overload candidate's checked body, reading its own already-
    /// resolved signature back rather than recomputing it.
    pub(crate) fn ensure_overload_body(&mut self, module_path: &[Ident], index: usize) -> Option<CheckedBody> {
        let key = (module_path.to_vec(), index);
        if let Some(body) = self.items.overload_bodies.get(&key) {
            return Some(body.clone_of());
        }
        let fn_type = self.ensure_overload_signature(module_path, index).ok()?;
        let hir = self.modules.hir(module_path);
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index confirmed to be a function");
        };
        let annotations = self.items.function_annotations.get(&f.id).cloned().unwrap_or_default();

        let run = self.with_analyzer(module_path, &[], (f.id, f.span), |analyzer| {
            analyzer.check_function_body(f, &fn_type, f.id, &annotations)
        });
        let body =
            CheckedBody { item: CheckedItem::FunctionDefinition(run.result?), warnings: run.warnings };
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
            a.params.iter().map(|(_, t)| t).eq(b.params.iter().map(|(_, t)| t))
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
                    AnalysisErrorKind::Redeclaration { name: name.clone(), previous: Some(previous) },
                ),
            );
        }
    }
}
