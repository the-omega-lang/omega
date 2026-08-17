//! Semantic analysis: everything between HIR and the checked tree.
//!
//! One [`Analyzer`] checks exactly one top-level item -- a signature or a
//! body -- and is thrown away afterwards. Everything module-shaped it needs
//! (what a path names, what an import means, another module's items) it asks
//! the [`ModuleResolver`] for; nothing here ever touches a filesystem or a
//! cross-module cache.
//!
//! The implementation is split by *what is being analyzed*, one submodule
//! per concern, each contributing its own `impl Analyzer` block:
//!
//! - [`visibility`] -- `exposed`/`internal`/hidden and the `reveal` bypass.
//! - [`specs`] -- spec declarations, flattening, conformance, and conformance.
//! - [`items`] -- top-level item signatures and bodies (the driver's entry
//!   points).
//! - [`stmts`] -- blocks, statements, control flow, divergence.
//! - [`exprs`] -- expression analysis and operators.
//! - [`literals`] -- number/struct/enum/union literals.
//! - [`places`] -- place expressions, field projection, slices, mutability.
//! - [`paths`] -- what a qualified or unqualified path names.
//! - [`calls`] -- callee resolution, overloads, generic calls, dispatch.
//! - [`patterns`] -- `match` and its exhaustiveness/narrowing.
//! - [`consts`] -- compile-time evaluation.
//!
//! Submodules import this module's own prelude with `use super::*` rather
//! than each repeating the same twenty-line import block: the canonical list
//! of what analysis depends on lives here, once.

mod calls;
mod consts;
mod exprs;
mod items;
mod literals;
mod paths;
mod patterns;
mod places;
mod specs;
mod stmts;
mod visibility;

pub use specs::PendingSpecMethod;
use specs::FlattenedSpecFn;

// Shared across submodules' own `use super::*`.
use calls::{CalleeResolution, Intercepted, Interceptor, ResolvedCallee};
use literals::parse_number_literal;

use crate::{
    checked::{
        CastKind, CheckedAddressOf, CheckedArrayLiteral, CheckedAssignment, CheckedBinaryOp,
        CheckedBlock, CheckedBreak, CheckedCast, CheckedContinue, CheckedDeclaration, CheckedDefer,
        CheckedDynamicCall, CheckedEnumConstruct, CheckedEnumDef, CheckedExpr, CheckedExprNode,
        CheckedExternDeclaration, CheckedFor, CheckedFunctionCall, CheckedFunctionDef, CheckedIf,
        CheckedLoop, CheckedMatch, CheckedMatchArm, CheckedParam, CheckedPlace, CheckedPlaceRoot,
        CheckedProjection, CheckedSlice, CheckedSpecCoerce, CheckedStmt, CheckedStructDef,
        CheckedStructLiteral, CheckedStructLiteralField, CheckedUnionConstruct, CheckedUnionDef,
        CheckedWhile, NumberValue, Storage,
    },
    context::{Context, ScopeContext, VarBinding},
    error::{
        AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind, TypeResolutionError,
    },
    generics::{resolve_inferred_type_args, unify_generic_type},
    resolved_type::{
        CastClass, ConformanceSource, ConstValue, NumericKind, RawSpecFunctionSig, ResolvedBound,
        ResolvedEnumType, ResolvedEnumVariant, ResolvedFunctionType, ResolvedMethod,
        ResolvedSpecType, ResolvedStructType, ResolvedType, ResolvedUnionType,
    },
    resolver::{
        GenericLiteralSignature, GenericSignature, GenericStaticFunctionSignature, ImportTarget,
        ItemNamespace, ModuleResolver, OverloadCandidates, ResolveError, ResolvedItem,
    },
    similarity::best_match,
};
use omega_hir::{
    BinaryOp, HirAddressOf, HirBlock, HirCast, HirCompoundAssign, HirDeclaration, HirEnumDef,
    HirExpr, HirExprNode, HirExternDeclaration, HirFor, HirForIn, HirFunctionCall, HirFunctionDef,
    HirId, HirIf, HirItem, HirMatch, HirMatchArm, HirParam, HirPattern, HirPlace, HirPlaceRoot,
    HirProjection, HirRange, HirSlice, HirSpecDef, HirStmt, HirStructDef, HirStructLiteral,
    HirStructLiteralField, HirUnionDef, HirWalrusDeclaration,
};
use omega_parser::prelude::{
    ExprPath, Ident, NumberBase, NumberExpr, Origin, Path, QualifiedSpecPath, SelfMode, Span,
    Type, Visibility,
};
use crate::target::Target;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub struct Analyzer<'r> {
    /// Private: every error goes through [`Analyzer::error`], so nothing
    /// outside this type ever pushes one directly.
    errors: Vec<AnalysisError>,
    /// Non-fatal findings -- currently just unreachable code (see
    /// `truncate_unreachable`) -- returned alongside a successful
    /// `CheckedModule` rather than folded into `errors`, since none of them
    /// reject the program. See `AnalysisWarning`'s doc comment.
    warnings: Vec<AnalysisWarning>,
    context: Context,
    /// The compilation target this analysis is being run for -- the
    /// *target's* pointer width (`pointer_bits`/`pointer_bytes`) is what
    /// every width-sensitive question (`numeric_kind`'s `ISize`/`USize`,
    /// `integer_domain`, `comp`'s `sizeof`) resolves against.
    target: Target,
    /// Everything module-tree-shaped -- filesystem lookups, cross-module
    /// caching, cycle detection -- lives entirely on the other side of this
    /// trait object (see `crate::resolver`); the same long-lived resolver
    /// (the driver) is borrowed across many short-lived per-module
    /// `Analyzer`s, one per `collect_signatures`/`analyze_bodies` call,
    /// rather than owned by any one of them.
    resolver: &'r mut dyn ModuleResolver,
    /// This item's owning module's absolute path -- supplies the implicit
    /// prefix an unqualified top-level reference needs to become an
    /// absolute `(module_path, name)` query, so it's resolved exactly the
    /// same way a qualified cross-module reference is (see
    /// `ModuleResolver::resolve_item`'s doc comment: there is no longer an
    /// architectural difference between the two). The *same* path for every
    /// item constructed for this module -- this module's own top-level
    /// signature/body work.
    module_path: Vec<Ident>,
    /// The enclosing function's declared return type, checked against every
    /// `return <expr>;` and against the function body's own effective type
    /// (see `block_type`/`check_function_return`). Reset at the start of
    /// each `check_function_body` call -- one `Analyzer` checks exactly one
    /// top-level item at a time (see `item_name`'s doc comment), and a
    /// struct's methods are checked sequentially, never nested inside one
    /// another's analysis, so a plain reset (not a save/restore) is enough.
    current_return_type: ResolvedType,
    /// A stack of enclosing loops' `HirId`s (innermost last), pushed/popped
    /// around a `while`/`for`'s body analysis. `break`/`continue` resolve
    /// against this -- today always `.last()` (the innermost loop), but
    /// looked up rather than hard-assumed specifically so a future labeled
    /// `break 'outer;` only has to change *this* resolution rule (search the
    /// stack for a matching label instead of always taking the top); nothing
    /// about `HirBreak`/`CheckedBreak`/codegen would need to change. Cleared
    /// at the start of each `check_function_body` call, same reasoning as
    /// `current_return_type`.
    loop_stack: Vec<HirId>,
    /// Every loop `HirId` that a `break` targeting it has actually been seen
    /// for, filled in as each `break` is analyzed (`HirStmt::Break`'s arm in
    /// `analyze_stmt`) and consulted once, when a `loop { }`'s own body
    /// finishes analyzing, to decide `CheckedLoop::has_break` -- see its own
    /// doc comment for why that's recorded on the checked node rather than
    /// re-derived later. IDs are never reused, so this only ever grows;
    /// never cleared, unlike `loop_stack`/`current_return_type`.
    loops_with_break: HashSet<HirId>,
    /// `true` while analyzing a `defer`'s own body (see `HirStmt::Defer`'s
    /// arm in `analyze_stmt`) -- not a stack/counter, since a `defer` nested
    /// inside another defer's body is rejected outright the moment this is
    /// already `true`, so it can never need to represent more than one
    /// level. Used to reject `return` inside a defer body (it would have to
    /// jump into the very epilogue that runs deferred bodies, from inside
    /// one of them) and nested `defer`. Reset at the start of each
    /// `check_function_body` call, same reasoning as `current_return_type`.
    in_defer_body: bool,
    /// A stack of `@suppress(...)` name lists, one frame per item/method
    /// currently being checked (innermost last) -- a method's own frame and
    /// its owning struct/enum/union's frame are both active while that
    /// method's body is checked, so either one suppresses a given warning
    /// (see `Analyzer::warn`), the same lexical-nesting behavior Rust's
    /// `#[allow]` has.
    suppressed: Vec<Vec<Ident>>,
    /// One frame per currently-active `reveal` expression (innermost last),
    /// each tracking whether *its* bypass has proven load-bearing yet (i.e.
    /// whether some check nested inside it would actually have failed
    /// without it) -- see `HirExpr::Reveal`'s analysis arm and
    /// `AnalysisWarningKind::UnnecessaryReveal`. `check_visibility` marks
    /// only the *innermost* frame (`.last_mut()`), so a redundant outer
    /// `reveal reveal x.y` still warns on the outer wrapper even though the
    /// inner one saved the access.
    reveal_stack: Vec<bool>,
    /// The struct/union/enum whose own method bodies this `Analyzer`
    /// instance is currently checking -- `Some(cell.borrow().id)` for the
    /// whole duration of `check_struct_body`/`check_union_body`/
    /// `check_enum_body` (one fresh `Analyzer` per type, never shared
    /// across two different types -- see those functions' own callers in
    /// `omega_driver::Driver`), `None` for a plain top-level function/
    /// global. This is what a **hidden field or method**'s own visibility
    /// rule is actually scoped to -- "cannot be accessed outside of the
    /// struct definition," a narrower scope than a hidden *item*'s
    /// (module-wide) rule, since it's compared against a single type's
    /// identity, not a module path. See `Analyzer::check_member_visibility`.
    current_owner: Option<HirId>,
    /// Field/variant usage recorded from `comp`-evaluated subtrees this
    /// `Analyzer` run interpreted (see `eval_comp`) -- they collapse into a
    /// `CheckedExpr::Const` and never reach the final `CheckedModule`, so
    /// `crate::dead_code::collect_module`'s own whole-program walk would
    /// otherwise never see the field accesses/enum constructions they
    /// contained. Folded into the driver-wide `FieldUsage` by
    /// `omega_driver::Driver::with_analyzer` once this `Analyzer` finishes.
    field_usage: crate::dead_code::FieldUsage,
    bounds: Vec<ResolvedBound>,
}

/// A top-level item's own name, or `None` for an `import` (which binds no
/// name of its own -- see `Context::import_module`/`bind_imported_item`
/// instead). Exposed for `omega_driver::Driver`, which now owns the
/// per-module "every named top-level item" index (`local_items`) that used
/// to live on `Analyzer` -- one item is resolved (and one `Analyzer`
/// constructed) at a time now, so there's no module-wide sweep left inside
/// this crate to share this with locally.
pub fn item_name(item: &HirItem) -> Option<Ident> {
    match item {
        HirItem::Declaration(d) => Some(d.ident.clone()),
        HirItem::DeclarationWithInit(d, _) => Some(d.ident.clone()),
        HirItem::Walrus(w) => Some(w.ident.clone()),
        HirItem::ExternDeclaration(d) => Some(d.ident.clone()),
        HirItem::FunctionDefinition(f) => Some(f.name.clone()),
        HirItem::Struct(s) => Some(s.name.clone()),
        HirItem::Enum(e) => Some(e.name.clone()),
        HirItem::Union(u) => Some(u.name.clone()),
        HirItem::Spec(sp) => Some(sp.name.clone()),
        HirItem::Gap(gap) => Some(gap.name.clone()),
        HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => None,
        HirItem::Import(_) => None,
    }
}

/// A top-level item's own declared `exposed`/`internal`/(default `Hidden`)
/// -- what `omega_driver::Driver::ensure_item` reads instead of hardcoding
/// `Visibility::Public`. `Import` has no visibility of its own (only
/// `reveal`, a different, use-site concept -- see `HirImport::reveal`) and
/// is never looked up through this path anyway (`item_name` already returns
/// `None` for it), so it's `unreachable!()` here rather than an arbitrary
/// default.
pub fn item_visibility(item: &HirItem) -> Visibility {
    match item {
        HirItem::Declaration(d) => d.visibility,
        HirItem::DeclarationWithInit(d, _) => d.visibility,
        HirItem::Walrus(w) => w.visibility,
        HirItem::ExternDeclaration(d) => d.visibility,
        HirItem::FunctionDefinition(f) => f.visibility,
        HirItem::Struct(s) => s.visibility,
        HirItem::Enum(e) => e.visibility,
        HirItem::Union(u) => u.visibility,
        HirItem::Spec(sp) => sp.visibility,
        HirItem::Gap(_) => Visibility::Exposed,
        HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => {
            unreachable!("unnamed blocks have no item visibility")
        }
        HirItem::Import(_) => {
            unreachable!("imports have no item-level visibility and are never looked up by name")
        }
    }
}

/// A top-level item's own `HirId`/`Span`, for anchoring a
/// `Redeclaration` error against a duplicate name -- see `item_name`.
pub fn item_id_span(item: &HirItem) -> (HirId, Span) {
    match item {
        HirItem::Declaration(d) => (d.id, d.span),
        HirItem::DeclarationWithInit(d, _) => (d.id, d.span),
        HirItem::Walrus(w) => (w.id, w.span),
        HirItem::ExternDeclaration(d) => (d.id, d.span),
        HirItem::FunctionDefinition(f) => (f.id, f.span),
        HirItem::Struct(s) => (s.id, s.span),
        HirItem::Enum(e) => (e.id, e.span),
        HirItem::Union(u) => (u.id, u.span),
        HirItem::Spec(sp) => (sp.id, sp.span),
        HirItem::Gap(gap) => (gap.id, gap.span),
        HirItem::Glue(glue) => (glue.id, glue.span),
        HirItem::Conform(conform) => (conform.id, conform.span),
        HirItem::Primitive(primitive) => (primitive.id, primitive.span),
        HirItem::Import(i) => (i.id, i.span),
    }
}

impl<'r> Analyzer<'r> {
    /// The lexical module for a written path. Macro-body paths are authored
    /// by their definition module; ordinary and substituted paths remain in
    /// the module currently being analyzed.
    pub(super) fn path_module(&self, path: &Path) -> Vec<Ident> {
        self.resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| self.module_path.clone())
    }

    /// Checks a resolved macro-body dependency against the macro's own
    /// declared visibility. Resolution still uses the definition module;
    /// this prevents a wider macro interface from exposing a narrower item.
    pub(super) fn check_macro_dependency_visibility(
        &mut self,
        id: HirId,
        span: Span,
        path: &Path,
        absolute: &[Ident],
    ) -> bool {
        let Some(macro_visibility) = self.resolver.macro_origin_visibility(path.origin) else {
            return true;
        };
        let Some(item_visibility) = self.resolver.declared_item_visibility(absolute) else {
            return true;
        };
        if item_visibility >= macro_visibility {
            return true;
        }
        self.error(
            id,
            span,
            AnalysisErrorKind::MacroDependencyTooPrivate {
                item: absolute.last().expect("absolute item path has a name").clone(),
                macro_visibility,
                item_visibility,
            },
        );
        false
    }

    /// Imports are no longer pre-resolved and pre-bound here: an `import`
    /// alias resolves lazily, the first time some name lookup that isn't
    /// satisfied locally actually needs to know what it means (see
    /// `Analyzer::resolve_alias`) -- this is what fixes a real false-cycle
    /// bug the old eager-resolve-the-whole-module's-imports-up-front
    /// approach had (two modules whose *unrelated* items happened to
    /// cross-import each other's module would deadlock on each other's
    /// whole import list, even though the specific items in question never
    /// referenced each other). `omega_driver::Driver` memoizes each
    /// `(module_path, alias)` alias resolution individually, so this
    /// throwaway `Analyzer` doesn't need its own import-alias cache either
    /// -- every lookup just asks the resolver directly.
    ///
    /// `generics` is the concrete substitution for the item's own declared
    /// generic parameters -- empty for an ordinary, non-generic item.
    /// Seeded into `defined_types`, with a `Redeclaration` for a duplicate
    /// entry within `generics` itself, anchored at `owner` -- the item's own
    /// id/span, since an individual generic parameter has none of its own.
    /// This is what makes a generic parameter nothing more than a type name
    /// bound to a concrete `ResolvedType` for the lifetime of one throwaway
    /// `Analyzer`: genericity is purely a resolution-time concern, matching
    /// the "duck typed" design (no bounds are ever declared or checked
    /// here).
    pub fn new(
        resolver: &'r mut dyn ModuleResolver,
        module_path: Vec<Ident>,
        generics: &[(Ident, ResolvedType)],
        owner: (HirId, Span),
        target: Target,
    ) -> Self {
        Self::new_in(resolver, module_path, generics, &[], owner, target)
    }

    pub fn new_in(
        resolver: &'r mut dyn ModuleResolver,
        module_path: Vec<Ident>,
        generics: &[(Ident, ResolvedType)],
        bounds: &[ResolvedBound],
        owner: (HirId, Span),
        target: Target,
    ) -> Self {
        let mut context = Context::new();
        let mut errors = Vec::new();

        let mut seen_generics = HashSet::new();
        for (ident, resolved_type) in generics {
            let dup = context.current_scope().defined_types.contains_key(ident)
                || !seen_generics.insert(ident);
            if dup {
                errors.push(AnalysisError::new(
                    owner.0,
                    owner.1,
                    AnalysisErrorKind::Redeclaration {
                        name: ident.clone(),
                        previous: None,
                    },
                ));
            } else {
                context
                    .current_scope()
                    .defined_types
                    .insert(ident.clone(), resolved_type.clone());
            }
        }

        Self {
            errors,
            warnings: vec![],
            context,
            resolver,
            module_path,
            target,
            current_return_type: ResolvedType::Void,
            loop_stack: vec![],
            loops_with_break: HashSet::new(),
            in_defer_body: false,
            suppressed: vec![],
            reveal_stack: vec![],
            current_owner: None,
            field_usage: crate::dead_code::FieldUsage::default(),
            bounds: bounds.to_vec(),
        }
    }

    /// The target's pointer width in bytes -- the one number every
    /// width-sensitive question outside this module (`annotations`,
    /// `comp_eval`) needs to ask.
    pub fn pointer_bytes(&self) -> u32 {
        self.target.pointer_bytes()
    }

    /// The target's pointer width in bits.
    pub fn pointer_bits(&self) -> u32 {
        self.target.pointer_bits()
    }

    /// Consumes this throwaway, per-item `Analyzer`, handing back whatever
    /// it accumulated -- `omega_driver::Driver` folds these into its own
    /// per-module `module_errors`/warnings after every signature/body call,
    /// and `field_usage` into its own whole-program `FieldUsage` accumulator
    /// (see `field_usage`'s own doc comment).
    pub fn finish(
        self,
    ) -> (
        Vec<AnalysisError>,
        Vec<AnalysisWarning>,
        crate::dead_code::FieldUsage,
    ) {
        (self.errors, self.warnings, self.field_usage)
    }

    /// Whether any currently active `@suppress(...)` frame (see
    /// `suppressed`'s doc comment) names this warning's stable slug (see
    /// `AnalysisWarningKind::name`).
    fn is_suppressed(&self, kind: &AnalysisWarningKind) -> bool {
        self.suppressed
            .iter()
            .any(|frame| frame.iter().any(|name| name.as_ref() == kind.name()))
    }

    /// The single choke point every error is pushed through -- the
    /// counterpart of `warn` below (which additionally honors `@suppress`;
    /// an error can never be suppressed).
    pub(crate) fn error(&mut self, node_id: HirId, span: Span, kind: AnalysisErrorKind) {
        self.errors.push(AnalysisError::new(node_id, span, kind));
    }

    /// The single choke point every warning is pushed through -- replaces
    /// a raw `self.warnings.push(AnalysisWarning::new(...))`, silently
    /// dropping the warning when `@suppress` has named it in an active
    /// frame instead.
    pub(crate) fn warn(&mut self, node_id: HirId, span: Span, kind: AnalysisWarningKind) {
        if !self.is_suppressed(&kind) {
            self.warnings
                .push(AnalysisWarning::new(node_id, span, kind));
        }
    }

    /// Runs `f` and discards every diagnostic it produced -- a speculative
    /// question whose failure is not this query's to report; the real path
    /// re-derives and reports it. Snapshots and truncates the *sink*, not
    /// per-diagnostic state, so everything pushed while `f` ran (errors,
    /// warnings, and anything a pushed error records on the side) is gone
    /// afterwards, exactly as if the probe never happened.
    ///
    /// Deliberately *not* retrofitted onto `classify_for_in_source` or
    /// `probe_literal_type_args`: those keep diagnostics on outright
    /// failure, which is a different contract -- this is discard-everything,
    /// for questions whose only consumer is the return value.
    pub fn probe<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let errors = self.errors.len();
        let warnings = self.warnings.len();
        let result = f(self);
        self.errors.truncate(errors);
        self.warnings.truncate(warnings);
        result
    }

    // Small generic fold used everywhere a list of HIR nodes is analyzed
    // into a list of checked ones: unlike a short-circuiting `?`/`collect`,
    // this keeps analyzing every item (so independent errors in the same
    // function/struct/module are all reported in one pass), and only
    // succeeds if every item did.
    fn analyze_all<T, U>(
        &mut self,
        items: &[T],
        mut f: impl FnMut(&mut Self, &T) -> Option<U>,
    ) -> Option<Vec<U>> {
        let mut checked = Vec::with_capacity(items.len());
        let mut ok = true;
        for item in items {
            match f(self, item) {
                Some(value) => checked.push(value),
                None => ok = false,
            }
        }
        ok.then_some(checked)
    }

    /// Runs `f` with `substitution` (`Self`, a spec's own generics, ... ->
    /// concrete types) pushed as a temporary scope, popped again afterward
    /// regardless of how `f` returns -- the shared "resolve a raw,
    /// unelaborated shape against a concrete substitution, without
    /// disturbing whatever's already bound in the calling implementor's own
    /// ambient `Context` (its own generics, already seeded when this
    /// `Analyzer` was constructed)" pattern every spec-flattening step
    /// needs: a function's own raw signature (`resolve_raw_spec_fn_type`)
    /// and a dependency's own raw type-argument list (`flatten_spec_into`).
    fn with_substitution<T>(
        &mut self,
        substitution: &[(Ident, ResolvedType)],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.context.enter_scope();
        for (name, ty) in substitution {
            self.context
                .current_scope()
                .defined_types
                .insert(name.clone(), ty.clone());
        }
        let result = f(self);
        self.context.leave_scope();
        result
    }

    /// Wraps `value` in `CheckedExpr::SpecCoerce` when `expected` is a
    /// `SpecObject` and `value` points to a type conforming to the target
    /// spec -- see
    /// `CheckedExpr::SpecCoerce`'s doc comment for why this needs an
    /// explicit node rather than being folded into `ResolvedType::accepts`
    /// itself. A no-op (returns `value` unchanged) whenever no such
    /// coercion applies -- including when `expected` already structurally
    /// `accepts` `value`, or when the spec isn't actually implemented (in
    /// which case the caller's own ordinary `accepts` check reports the
    /// mismatch exactly as before, just without this specific "why" -- an
    /// accepted simplification, not every coercion site needs its own
    /// bespoke diagnostic) -- the latter also covers a satisfying method
    /// that exists but isn't visible enough from here, see
    /// `type_implements_spec`'s `check_method_visibility` doc.
    fn coerce_to_expected(
        &mut self,
        expected: Option<&ResolvedType>,
        value: CheckedExprNode,
    ) -> CheckedExprNode {
        let Some(
            target @ ResolvedType::SpecObject {
                spec,
                type_args,
                mutable: expected_mutable,
            },
        ) = expected
        else {
            return value;
        };
        if target.accepts(&value.r#type) {
            return value;
        }
        let ResolvedType::Pointer {
            pointee,
            mutable: value_mutable,
        } = &value.r#type
        else {
            return value;
        };
        if !*value_mutable && *expected_mutable {
            return value;
        }
        let Ok(slots) =
            self.type_implements_spec(value.id, value.span, pointee, spec, type_args, true)
        else {
            return value;
        };
        CheckedExprNode {
            id: value.id,
            span: value.span,
            r#type: target.clone(),
            kind: CheckedExpr::SpecCoerce(CheckedSpecCoerce {
                base: Box::new(value),
                slots,
            }),
        }
    }

    /// Binds `ident` in the current scope, or records `Redeclaration` if
    /// it's already bound there. Centralizes what used to be, incorrectly, a
    /// codegen-side check on a name-keyed stack-slot map.
    fn declare_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        r#type: ResolvedType,
        storage: Storage,
        mutable: bool,
    ) -> Option<()> {
        self.declare_binding_impl(
            ident,
            origin,
            VarBinding {
                decl_id: id,
                storage,
                r#type,
                span,
                narrowed: false,
                mutable,
                used: false,
                written: false,
            },
        )
    }

    /// `comp ident := comp expr;` -- binds `ident` with `Storage::Comp`
    /// (never `mutable`: see `AnalysisErrorKind::MutCompBinding`, checked
    /// by the caller before this runs) and records its already-evaluated
    /// `value` in `Context::comp_values`, so `analyze_place_read` can
    /// substitute every later reference with it directly.
    fn declare_comp_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        r#type: ResolvedType,
        value: crate::resolved_type::ConstValue,
    ) -> Option<()> {
        self.declare_binding(id, span, ident, origin, r#type, Storage::Comp, false)?;
        self.context.set_comp_value(id, value);
        Some(())
    }

    /// See `VarBinding::narrowed`'s doc comment -- used only by
    /// `analyze_enum_match` to shadow-declare a matched arm's narrowed
    /// scrutinee. `mutable` is inherited from the binding being narrowed
    /// (reassigning the narrowed view is exactly as valid as reassigning
    /// the original would have been).
    fn declare_narrowed_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        r#type: ResolvedType,
        storage: Storage,
        mutable: bool,
    ) -> Option<()> {
        self.declare_binding_impl(
            ident,
            origin,
            VarBinding {
                decl_id: id,
                storage,
                r#type,
                span,
                narrowed: true,
                mutable,
                used: false,
                written: false,
            },
        )
    }

    /// Adds one binding to the current scope, rejecting a name that scope
    /// already binds.
    fn declare_binding_impl(
        &mut self,
        ident: &Ident,
        origin: Origin,
        binding: VarBinding,
    ) -> Option<()> {
        let (id, span) = (binding.decl_id, binding.span);
        match self.context.current_scope().declare(ident.clone(), origin, binding) {
            Ok(()) => Some(()),
            Err((name, previous)) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::Redeclaration {
                        name,
                        previous: Some(previous),
                    },
                );
                None
            }
        }
    }

    /// The one choke point every ordinary `ModuleResolver::resolve_item`
    /// call goes through -- computes `bypass` from `reveal_stack`, calls
    /// through with `self.module_path` as the accessor, and on a bypassed
    /// success, consults `is_item_visible` to mark the innermost `reveal`
    /// frame load-bearing (the same "was this bypass actually necessary"
    /// tracking `check_visibility` does for in-analyzer checks, just across
    /// the `ModuleResolver` trait boundary -- see `AnalysisWarningKind::
    /// UnnecessaryReveal`). Every other argument passes straight through
    /// unchanged; this exists purely to avoid repeating the bypass/mark
    /// dance at each of this crate's ~10 call sites.
    fn resolve_item_checked(
        &mut self,
        absolute: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let bypass = !self.reveal_stack.is_empty();
        let result =
            self.resolver
                .resolve_item(&self.module_path, absolute, type_args, indirect, bypass);
        if bypass && result.is_ok() && !self.resolver.is_item_visible(&self.module_path, absolute) {
            *self
                .reveal_stack
                .last_mut()
                .expect("bypass true implies a non-empty reveal_stack") = true;
        }
        result
    }

    /// `resolve_item_checked`, plus one extra retry against every exposed
    /// name in `core`'s own tree (see `ModuleResolver::
    /// ambient_core_candidates`'s doc comment -- `core` is a full ambient
    /// prelude, not the short, hardcoded table this used to be) when
    /// `absolute` names an unqualified single segment that didn't resolve
    /// locally. `prefix` is the original, pre-absolute-path segment list a
    /// caller built `absolute` from (`generic_prefix_absolute`'s own
    /// input) -- needed because `absolute` alone can't tell "this was
    /// genuinely unqualified" apart from "this happens to produce a
    /// same-shaped absolute path", so the fallback can only be judged safe
    /// by whoever still has `prefix` around, not by `resolve_item_checked`
    /// after the fact.
    fn resolve_item_checked_with_ambient_fallback(
        &mut self,
        prefix: &[Ident],
        absolute: &[Ident],
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let result = self.resolve_item_checked(absolute, type_args, true);
        match (prefix, &result) {
            ([single], Err(ResolveError::UnknownItem { .. })) => {
                match self
                    .resolver
                    .ambient_core_candidates(&self.module_path, single)?
                {
                    Some(ambient) => self.resolve_item_checked(&ambient, type_args, true),
                    None => result,
                }
            }
            _ => result,
        }
    }

    /// The definition-site equivalent of
    /// `resolve_item_checked_with_ambient_fallback`.  Macro-authored paths
    /// must use the macro's module as both their lookup root and visibility
    /// accessor; a caller's `reveal` must not leak into a macro body.
    fn resolve_item_with_ambient_from(
        &mut self,
        accessor: &[Ident],
        prefix: &[Ident],
        absolute: &[Ident],
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let result = self.resolver.resolve_item(accessor, absolute, type_args, true, false);
        match (prefix, &result) {
            ([single], Err(ResolveError::UnknownItem { .. })) => {
                match self.resolver.ambient_core_candidates(accessor, single)? {
                    Some(ambient) => self.resolver.resolve_item(accessor, &ambient, type_args, true, false),
                    None => result,
                }
            }
            _ => result,
        }
    }

    /// `indirect` is true whenever `typ` sits somewhere that never embeds
    /// its referent inline into another type's layout -- a function's own
    /// param/return types, or anything already behind a `Pointer`/`Array`/
    /// `Slice` -- as opposed to a struct field or `SizedArray` element,
    /// which do. See `ModuleResolver::resolve_item`'s doc comment for why
    /// this distinction is what separates a legitimate self-reference
    /// (`next: *Node`) from a genuine infinite-size cycle (`value: Node`).
    /// The on-demand triggering that used to happen in a separate pre-pass
    /// here now happens inline, inside `Context::resolve_type` itself (it
    /// calls the resolver directly on an unqualified miss), so this is just
    /// a thin error-reporting wrapper around it.
    pub(crate) fn resolve_type_or_error(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
    ) -> Option<ResolvedType> {
        self.resolve_type_or_error_checked(id, span, typ, indirect, false)
    }

    /// The one place a function/method/extern/gap's own declared return
    /// type is resolved -- identical to `resolve_type_or_error`, except a
    /// bare `never` is the expected, successful result here instead of a
    /// mistake (see `TypeResolutionError::NeverNotAllowedHere`'s doc
    /// comment). Every *other* type position -- a local, a field, a
    /// parameter, a `(...) => T` function type's own inner return-type
    /// slot (resolved directly by `Context::resolve_type`, never through
    /// this wrapper at all) -- goes through the ordinary
    /// `resolve_type_or_error` instead, which continues to reject it.
    pub(crate) fn resolve_return_type_or_error(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
    ) -> Option<ResolvedType> {
        self.resolve_type_or_error_checked(id, span, typ, indirect, true)
    }

    /// `resolve_return_type_or_error`, resolved against an explicit module
    /// -- see `resolve_type_or_error_in`'s doc comment (definition-site
    /// resolution for a spec's own raw signatures).
    pub(crate) fn resolve_return_type_or_error_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        self.resolve_type_or_error_checked_in(id, span, typ, indirect, true, module)
    }

    fn resolve_type_or_error_checked_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        allow_never: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        let resolved = self.resolve_type_or_error_in(id, span, typ, indirect, module)?;
        // A bare spec name (`ResolvedType::Spec`) is never a valid value
        // type -- see `TypeResolutionError::SpecUsedAsValueType`'s doc
        // comment. Every position that legitimately wants one (a conform
        // declaration, a generic bound, `spec *Foo`'s own pointee) goes through
        // `resolve_spec_reference`, which calls `resolve_type_or_error_raw`
        // directly instead of this wrapper -- so every other caller (which
        // is every caller reached through here) is asking for an actual
        // value type, and a bare spec is always a mistake.
        if let ResolvedType::Spec(spec) = &resolved {
            let name = spec.borrow().name.clone();
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::SpecUsedAsValueType(name)),
            );
            return None;
        }
        if !allow_never && resolved == ResolvedType::Never {
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::NeverNotAllowedHere),
            );
            return None;
        }
        Some(resolved)
    }

    fn resolve_type_or_error_checked(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        allow_never: bool,
    ) -> Option<ResolvedType> {
        let resolved = self.resolve_type_or_error_raw(id, span, typ, indirect)?;
        // A bare spec name (`ResolvedType::Spec`) is never a valid value
        // type -- see `TypeResolutionError::SpecUsedAsValueType`'s doc
        // comment. Every position that legitimately wants one (a conform
        // declaration, a generic bound, `spec *Foo`'s own pointee) goes through
        // `resolve_spec_reference`, which calls `resolve_type_or_error_raw`
        // directly instead of this wrapper -- so every other caller (which
        // is every caller reached through here) is asking for an actual
        // value type, and a bare spec is always a mistake.
        if let ResolvedType::Spec(spec) = &resolved {
            let name = spec.borrow().name.clone();
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::SpecUsedAsValueType(name)),
            );
            return None;
        }
        if !allow_never && resolved == ResolvedType::Never {
            self.error(
                id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::NeverNotAllowedHere),
            );
            return None;
        }
        Some(resolved)
    }

    /// The same resolution `resolve_type_or_error` does, minus its
    /// bare-`ResolvedType::Spec`-is-never-a-value-type check -- for the one
    /// legitimate exception, `resolve_spec_reference` (a conform declaration
    /// entry, a spec dependency, a generic bound), where a bare spec is
    /// exactly the expected, successful result.
    pub(crate) fn resolve_type_or_error_raw(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
    ) -> Option<ResolvedType> {
        let module = self.module_path.clone();
        self.resolve_type_or_error_in(id, span, typ, indirect, &module)
    }

    /// `resolve_type_or_error_raw`, resolved against an explicit module --
    /// the definition-site resolution a spec's own raw function signature
    /// needs when the flatten runs from a caller's module (`Analyzer::
    /// flatten_spec_into`): a spec's types are written in the spec's own
    /// module and resolve there, wherever the spec happens to be *used*
    /// from.
    pub(crate) fn resolve_type_or_error_in(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        indirect: bool,
        module: &[Ident],
    ) -> Option<ResolvedType> {
        let bypass = !self.reveal_stack.is_empty();
        match self.context.resolve_type(
            typ.to_owned(),
            &mut *self.resolver,
            module,
            indirect,
            bypass,
        ) {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                self.error(id, span, AnalysisErrorKind::UnresolvedType(err));
                None
            }
        }
    }

    /// Resolves `typ` with `subst`'s bindings visible as if they were
    /// already-bound generic parameters, on top of whatever this analyzer's
    /// own scope already has -- one shared primitive behind two different
    /// default-generic resolution sites: `ensure_item`'s padding gate (a
    /// throwaway `Analyzer` already seeded with everything resolved so far
    /// via `Analyzer::new`, called with `subst` empty), and this analyzer's
    /// own eager, per-argument precedence resolution in
    /// `Analyzer::infer_generic_args` (a live analyzer mid-call, where
    /// `subst` is the partial substitution built up so far and nothing is
    /// pre-seeded). `pub`, not `pub(crate)`, specifically so
    /// `omega_driver::items::ensure_item` can call it through
    /// `Driver::with_analyzer` -- every other type-resolution entry point
    /// here stays crate-private.
    pub fn resolve_under_substitution(
        &mut self,
        id: HirId,
        span: Span,
        typ: &Type,
        subst: &[(Ident, ResolvedType)],
    ) -> Option<ResolvedType> {
        self.context
            .enter_scope()
            .defined_types
            .extend(subst.iter().cloned());
        let result = self.resolve_type_or_error(id, span, typ, false);
        self.context.leave_scope();
        result
    }

    /// Infers `generics` from `params`/`args`, analyzed strictly left to
    /// right -- the machinery behind "expected type > explicit type >
    /// declared default > inference," the precedence rule a generic
    /// function/static-call argument follows (the expected type seeds the
    /// substitution *before* any argument is analyzed, the same order
    /// `infer_literal_type_args` already uses for struct literals). Shared
    /// by `Analyzer::finish_generic_call` and `finish_generic_static_call`;
    /// `probe_literal_type_args` keeps the identical per-slot shape (matched
    /// by field name there instead of position) with its own empty seed.
    ///
    /// `seed` is the substitution this inference starts from -- every entry
    /// behaves exactly like a generic already pinned by an earlier
    /// argument. Before each argument is analyzed,
    /// `expected_for_generic_param` substitutes its raw declared type
    /// against whatever's already bound (the seed, then earlier arguments in
    /// this same call) or, for a still-unbound generic with its own declared
    /// default, that default -- giving `analyze_expr` a real `expected` a
    /// bare literal can adapt to, the same "earliest position is the
    /// anchor" precedent `if`-branches and binary operands already use (see
    /// `docs/03-control-flow.md`). The argument's own *actual* resolved type
    /// -- not the tentative `expected` -- is what `unify_generic_type` then
    /// permanently pins the generic to: an explicit suffix/type on the
    /// argument always wins over `expected` regardless (see
    /// `analyze_number`), so this never needs to track "was this pinned by
    /// a default" separately from an ordinary argument-driven pin. A later
    /// argument whose own explicit type conflicts with an already-pinned
    /// generic is left to the caller's own, unchanged final `accepts` loop
    /// to reject.
    ///
    /// Returns every argument, checked, plus the resulting (possibly
    /// partial) substitution -- a generic that never appears in any
    /// parameter type at all (return-type-only, or every generic on a
    /// zero-arg call) is left unbound here for the caller's own
    /// `resolve_inferred_type_args` call to either default (if it has one)
    /// or error on.
    pub(crate) fn infer_generic_args(
        &mut self,
        generics: &[Ident],
        defaults: &[Option<Type>],
        params: &[Type],
        args: &[HirExprNode],
        seed: HashMap<Ident, ResolvedType>,
    ) -> Option<(Vec<CheckedExprNode>, HashMap<Ident, ResolvedType>)> {
        let mut subst = seed;
        let mut checked_args = Vec::with_capacity(args.len());
        for (raw_type, arg) in params.iter().zip(args) {
            let expected = self
                .expected_for_generic_param(arg.id, arg.span, raw_type, generics, defaults, &subst);
            let checked = self.analyze_expr(arg, expected.as_ref())?;
            unify_generic_type(generics, raw_type, &checked.r#type, &mut subst);
            checked_args.push(checked);
        }
        Some((checked_args, subst))
    }

    /// `raw_type` resolved under `subst`, treating any generic in
    /// `generics` that's unbound in `subst` but has its own declared
    /// default as if it were already bound to that default (resolved
    /// recursively under the same growing substitution, so `B = A` can see
    /// whatever `A` is already bound or defaulted to). `None` -- meaning
    /// "no hint, fall back to ordinary inference," entirely unchanged --
    /// whenever `raw_type` references a generic with neither a binding nor
    /// a default; deliberately checked structurally *before* attempting any
    /// resolution, so this never mistakes an as-yet-truly-unconstrained
    /// generic name for an ordinary unresolved type and reports a spurious
    /// error for it.
    pub(crate) fn expected_for_generic_param(
        &mut self,
        id: HirId,
        span: Span,
        raw_type: &Type,
        generics: &[Ident],
        defaults: &[Option<Type>],
        subst: &HashMap<Ident, ResolvedType>,
    ) -> Option<ResolvedType> {
        if !Self::generic_refs_resolvable(raw_type, generics, defaults, subst) {
            return None;
        }
        let mut local: Vec<(Ident, ResolvedType)> =
            subst.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (generic, default) in generics.iter().zip(defaults) {
            if subst.contains_key(generic) {
                continue;
            }
            let Some(default) = default else { continue };
            let resolved = self.resolve_under_substitution(id, span, default, &local)?;
            local.push((generic.clone(), resolved));
        }
        self.resolve_under_substitution(id, span, raw_type, &local)
    }

    /// Whether every generic `raw_type` references (anywhere in its shape)
    /// is either already bound in `subst` or has its own declared default
    /// -- the purely structural check `expected_for_generic_param` runs
    /// before ever attempting real resolution. Mirrors
    /// `type_references_generics`'s own recursive walk.
    fn generic_refs_resolvable(
        raw_type: &Type,
        generics: &[Ident],
        defaults: &[Option<Type>],
        subst: &HashMap<Ident, ResolvedType>,
    ) -> bool {
        let name_ok = |name: &Ident| match generics.iter().position(|g| g == name) {
            Some(i) => subst.contains_key(name) || defaults[i].is_some(),
            None => true,
        };
        match raw_type {
            Type::Named(path) => !path.is_unqualified() || name_ok(&path.head),
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => {
                Self::generic_refs_resolvable(inner, generics, defaults, subst)
            }
            Type::Generic(path, args) => {
                (!path.is_unqualified() || name_ok(&path.head))
                    && args
                        .iter()
                        .all(|a| Self::generic_refs_resolvable(a, generics, defaults, subst))
            }
            Type::SpecObject(inner, _) => {
                Self::generic_refs_resolvable(inner, generics, defaults, subst)
            }
            Type::SpecStatic(inner) => {
                Self::generic_refs_resolvable(inner, generics, defaults, subst)
            }
            Type::Function(f) => {
                f.params
                    .iter()
                    .all(|(_, p)| Self::generic_refs_resolvable(p, generics, defaults, subst))
                    && Self::generic_refs_resolvable(&f.return_type, generics, defaults, subst)
            }
        }
    }
}
