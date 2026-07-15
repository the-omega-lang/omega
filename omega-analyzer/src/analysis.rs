use crate::{
    checked::{
        CheckedAddressOf, CheckedArrayLiteral, CheckedAssignment, CheckedBinaryOp, CheckedBlock,
        CheckedBreak, CheckedCast, CheckedContinue, CheckedDeclaration, CheckedDefer, CheckedDynamicCall,
        CheckedEnumConstruct,
        CheckedEnumDef, CheckedExpr,
        CheckedExprNode, CheckedExternDeclaration, CheckedFor, CheckedFunctionCall, CheckedFunctionDef,
        CheckedIf, CheckedMatch, CheckedMatchArm,
        CheckedParam, CheckedPlace, CheckedPlaceRoot,
        CheckedProjection, CheckedSlice, CheckedSpecCoerce, CheckedStmt, CheckedStructDef, CheckedStructLiteral,
        CheckedStructLiteralField, CheckedUnionConstruct, CheckedUnionDef, CheckedWhile, CastKind, NumberValue,
        Storage,
    },
    context::{Context, VarBinding},
    error::{AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind, TypeResolutionError},
    generics::unify_generic_type,
    resolved_type::{
        CastClass, ConstValue, NumericKind, RawSpecFunctionSig, ResolvedEnumType, ResolvedEnumVariant,
        ResolvedFunctionType, ResolvedMethod, ResolvedSpecType, ResolvedStructType, ResolvedType, ResolvedUnionType,
    },
    resolver::{GenericSignature, ImportTarget, ItemNamespace, ModuleResolver, ResolveError, ResolvedItem},
    similarity::best_match,
};
use omega_hir::{
    BinaryOp, HirAddressOf, HirBlock, HirCast, HirCompoundAssign, HirDeclaration, HirEnumDef, HirExpr, HirExprNode,
    HirExternDeclaration,
    HirFor, HirFunctionCall, HirFunctionDef, HirId, HirIf, HirItem, HirMatch, HirPattern, HirParam,
    HirPlace, HirPlaceRoot, HirProjection, HirRange, HirSlice, HirSpecDef, HirStmt, HirStructDef, HirStructLiteral,
    HirUnionDef, HirWalrusDeclaration,
};
use omega_parser::prelude::{ExprPath, Ident, NumberBase, NumberExpr, Span, Type};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A function-call's callee, resolved to either an ordinary value (whose
/// type must be `Function`) or a bound method reference (a "thiscall"):
/// `base.method(args)` where `method` names a struct method rather than a
/// field becomes an ordinary call to the method with `&base` (or, if `base`
/// was already a pointer, `base` itself) prepended as the first (`self`)
/// argument -- `HirFunctionDef`'s synthetic `self` parameter (see
/// `omega_hir::lower::lower_function_def`) already accounts for it in
/// `fn_type`, so no special-casing is needed in the argument-checking loop
/// in `FunctionCall` handling.
struct ResolvedCallee {
    callee: CheckedExprNode,
    fn_type: ResolvedFunctionType,
    implicit_self: Option<CheckedExprNode>,
    /// `Some` only when `method` named 2+ overloaded candidates -- overload
    /// resolution (`Analyzer::resolve_overload`) already had to fully
    /// analyze (and pick the concrete type of) every user-written argument
    /// itself, to score candidates, so those are handed back here instead
    /// of asking `FunctionCall`'s own argument loop to redo (and
    /// potentially re-error on) the same work. `None` -- the overwhelming
    /// majority of calls -- means the ordinary loop runs exactly as before.
    checked_args: Option<Vec<CheckedExprNode>>,
}

/// `resolve_callee`'s real result: either an ordinary callee (the ordinary
/// case, handled by the `FunctionCall` arm's own existing argument loop)
/// or a fully-resolved dynamic-dispatch call (`base.method(...)` where
/// `base`'s type is a `spec *Spec` value) -- built entirely inside
/// `resolve_callee` itself, since a dynamic call's shape has no ordinary
/// "callee expression" to hand back at all (see `CheckedExpr::DynamicCall`).
/// Folding this into `resolve_callee` itself, rather than a separate
/// sibling interceptor (like `resolve_overloaded_call`'s), is deliberate:
/// every interceptor's `None`-means-"not applicable" contract requires a
/// cheap, side-effect-free peek, but telling a dynamic call apart from an
/// ordinary one needs the base place's *resolved type* -- exactly what
/// `resolve_callee` already computes, once, via `analyze_place`. A second,
/// separate `analyze_place` call on the same base would risk reporting a
/// broken base's own errors twice.
enum CalleeResolution {
    Ordinary(ResolvedCallee),
    Dynamic(Option<CheckedExprNode>),
}

/// What a `Name { ... }` literal's path resolved to -- see
/// `Analyzer::resolve_literal_target`.
enum LiteralTarget {
    /// Always wraps `ResolvedType::Struct`.
    Struct(ResolvedType),
    EnumVariant(Rc<RefCell<ResolvedEnumType>>, usize),
    /// Always wraps `ResolvedType::Union`.
    Union(ResolvedType),
}

/// One spec function requirement, flattened out of a (possibly generic,
/// possibly multiply-inherited) spec reference and resolved for one
/// specific concrete implementor -- see `Analyzer::flatten_spec`.
struct FlattenedSpecFn {
    name: Ident,
    fn_type: ResolvedFunctionType,
    raw: RawSpecFunctionSig,
    spec_name: Ident,
    /// `Self` + the owning spec's own generics, bound to concrete types --
    /// exactly what resolved `fn_type` above, kept around so a queued
    /// default instantiation's *body* can be checked later (phase 2, see
    /// `PendingSpecMethod`) with the identical substitution its signature
    /// already used.
    substitution: Vec<(Ident, ResolvedType)>,
}

/// A spec-default method an implementor needs (no override, spec supplied
/// a body) -- signature already resolved and merged into the implementor's
/// `functions` list in phase 1 (`Analyzer::resolve_implements_clause`);
/// this is only what phase 2 (`check_struct_body`/`_enum`/`_union`) still
/// needs to check the body itself with the same `Self`/generics binding --
/// see `Analyzer::check_pending_spec_method`.
#[derive(Clone)]
pub struct PendingSpecMethod {
    pub id: HirId,
    pub fn_type: ResolvedFunctionType,
    pub raw: RawSpecFunctionSig,
    pub substitution: Vec<(Ident, ResolvedType)>,
}

pub struct Analyzer<'r> {
    errors: Vec<AnalysisError>,
    /// Non-fatal findings -- currently just unreachable code (see
    /// `truncate_unreachable`) -- returned alongside a successful
    /// `CheckedModule` rather than folded into `errors`, since none of them
    /// reject the program. See `AnalysisWarning`'s doc comment.
    warnings: Vec<AnalysisWarning>,
    context: Context,
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
    /// `true` while analyzing a `defer`'s own body (see `HirStmt::Defer`'s
    /// arm in `analyze_stmt`) -- not a stack/counter, since a `defer` nested
    /// inside another defer's body is rejected outright the moment this is
    /// already `true`, so it can never need to represent more than one
    /// level. Used to reject `return` inside a defer body (it would have to
    /// jump into the very epilogue that runs deferred bodies, from inside
    /// one of them) and nested `defer`. Reset at the start of each
    /// `check_function_body` call, same reasoning as `current_return_type`.
    in_defer_body: bool,
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
        HirItem::ExternDeclaration(d) => Some(d.ident.clone()),
        HirItem::FunctionDefinition(f) => Some(f.name.clone()),
        HirItem::Struct(s) => Some(s.name.clone()),
        HirItem::Enum(e) => Some(e.name.clone()),
        HirItem::Union(u) => Some(u.name.clone()),
        HirItem::Spec(sp) => Some(sp.name.clone()),
        HirItem::Import(_) => None,
    }
}

/// A top-level item's own `HirId`/`Span`, for anchoring a
/// `Redeclaration` error against a duplicate name -- see `item_name`.
pub fn item_id_span(item: &HirItem) -> (HirId, Span) {
    match item {
        HirItem::Declaration(d) => (d.id, d.span),
        HirItem::ExternDeclaration(d) => (d.id, d.span),
        HirItem::FunctionDefinition(f) => (f.id, f.span),
        HirItem::Struct(s) => (s.id, s.span),
        HirItem::Enum(e) => (e.id, e.span),
        HirItem::Union(u) => (u.id, u.span),
        HirItem::Spec(sp) => (sp.id, sp.span),
        HirItem::Import(i) => (i.id, i.span),
    }
}

/// The pure parse-and-range-check core behind a number literal's concrete
/// value -- no `Span`/error-pushing, just `Err(())` on failure, so this is
/// equally usable from `Analyzer::analyze_number`'s real (error-reporting)
/// path and from overload-viability scoring's *silent* "would this literal
/// fit this candidate" check (a rejected candidate must never push a
/// speculative error). `kind` is whatever concrete numeric type the caller
/// already decided on (explicit suffix, inferred from context, or the
/// plain i32/f64 default) -- this never picks the type itself, only
/// validates the literal's digits against it.
fn parse_number_literal(n: &NumberExpr, kind: NumericKind) -> Result<NumberValue, ()> {
    match kind {
        NumericKind::Float(width) => {
            let text = format!("{}.{}", n.integer_part, n.fractional_part.as_deref().unwrap_or("0"));
            let parsed = text.parse::<f64>().map_err(|_| ())?;
            if width == 32 && parsed.is_finite() && (parsed as f32).is_infinite() {
                return Err(());
            }
            Ok(NumberValue::Float(parsed))
        }
        NumericKind::Signed(width) => {
            let parsed = u64::from_str_radix(&n.integer_part, n.base.radix()).map_err(|_| ())?;
            let max = if width == 64 { i64::MAX as u64 } else { (1u64 << (width - 1)) - 1 };
            if parsed > max {
                return Err(());
            }
            Ok(NumberValue::Signed(parsed as i64))
        }
        NumericKind::Unsigned(width) => {
            let parsed = u64::from_str_radix(&n.integer_part, n.base.radix()).map_err(|_| ())?;
            let max = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
            if parsed > max {
                return Err(());
            }
            Ok(NumberValue::Unsigned(parsed))
        }
    }
}

impl<'r> Analyzer<'r> {
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
    ) -> Self {
        let mut context = Context::new();
        let mut errors = Vec::new();

        let mut seen_generics = HashSet::new();
        for (ident, resolved_type) in generics {
            let dup = context.current_scope().defined_types.contains_key(ident) || !seen_generics.insert(ident);
            if dup {
                errors.push(AnalysisError::new(
                    owner.0,
                    owner.1,
                    AnalysisErrorKind::Redeclaration { name: ident.clone(), previous: None },
                ));
            } else {
                context.current_scope().defined_types.insert(ident.clone(), resolved_type.clone());
            }
        }

        Self {
            errors,
            warnings: vec![],
            context,
            resolver,
            module_path,
            current_return_type: ResolvedType::Void,
            loop_stack: vec![],
            in_defer_body: false,
        }
    }

    /// Consumes this throwaway, per-item `Analyzer`, handing back whatever
    /// it accumulated -- `omega_driver::Driver` folds these into its own
    /// per-module `module_errors`/warnings after every signature/body call.
    pub fn finish(self) -> (Vec<AnalysisError>, Vec<AnalysisWarning>) {
        (self.errors, self.warnings)
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
    fn resolve_type_or_error(&mut self, id: HirId, span: Span, typ: &Type, indirect: bool) -> Option<ResolvedType> {
        match self.context.resolve_type(typ.to_owned(), &mut *self.resolver, &self.module_path, indirect) {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                self.errors
                    .push(AnalysisError::new(id, span, AnalysisErrorKind::UnresolvedType(err)));
                None
            }
        }
    }

    /// Resolves a raw `Type` that's expected to name a spec (an implements
    /// clause entry, a spec dependency, a generic bound) to its cell plus
    /// its own resolved generic arguments (e.g. `Iterator<i32>`'s `[i32]`)
    /// -- `None` on failure (already reported, either as an ordinary
    /// `UnresolvedType` or, if it resolved to something other than a spec,
    /// `TypeResolutionError::NotASpec`).
    fn resolve_spec_reference(
        &mut self,
        id: HirId,
        span: Span,
        ty: &Type,
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        let raw_args: Vec<Type> = match ty {
            Type::Generic(_, args) => args.clone(),
            _ => vec![],
        };
        let mut resolved_args = Vec::with_capacity(raw_args.len());
        let mut ok = true;
        for arg in &raw_args {
            match self.resolve_type_or_error(id, span, arg, true) {
                Some(r) => resolved_args.push(r),
                None => ok = false,
            }
        }
        let name = match ty {
            Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
            _ => Ident("<spec>".to_string()),
        };
        let resolved = self.resolve_type_or_error(id, span, ty, true)?;
        if !ok {
            return None;
        }
        match resolved {
            ResolvedType::Spec(spec) => Some((spec, resolved_args)),
            _ => {
                self.errors.push(AnalysisError::new(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name)),
                ));
                None
            }
        }
    }

    /// Builds a spec's own raw (unresolved) function signature list --
    /// `RawSpecFunctionSig`'s doc comment explains why no type resolution
    /// happens here at all (deferred to `flatten_spec`, once a concrete
    /// implementor's `Self` is known). Checks only for a duplicate name
    /// among the spec's own functions -- a genuine signature conflict
    /// between *dependencies* is a `flatten_spec`-time concern instead
    /// (only detectable once both sides are resolved with a concrete
    /// `Self`).
    pub fn resolve_spec_functions(&mut self, sp: &HirSpecDef) -> Vec<(Ident, RawSpecFunctionSig)> {
        let mut functions = Vec::new();
        let mut seen: HashSet<Ident> = HashSet::new();
        for f in &sp.functions {
            if !seen.insert(f.name.clone()) {
                self.errors.push(AnalysisError::new(
                    f.id,
                    f.span,
                    AnalysisErrorKind::Redeclaration { name: f.name.clone(), previous: None },
                ));
                continue;
            }
            functions.push((
                f.name.clone(),
                RawSpecFunctionSig {
                    decl_id: f.id,
                    name: f.name.clone(),
                    span: f.span,
                    is_member_function: f.is_member_function,
                    params: f.params.clone(),
                    return_type: f.return_type.clone(),
                    default_body: f.body.clone(),
                },
            ));
        }
        functions
    }

    /// Resolves a spec's own declared dependency list (`spec Mammal :
    /// Animal, Dummy`) to their cells + resolved type args -- see
    /// `ResolvedSpecType::dependencies`'s doc comment for why these are
    /// eagerly, fully resolved (unlike function signatures): a dependency
    /// can't forward the *depending* spec's own still-abstract generics
    /// into its own type arguments in this design (a documented scope
    /// boundary, not an oversight) -- `spec Foo<T> : Bar<i32>` resolves
    /// fine, `spec Foo<T> : Bar<T>` reports `T` as an unrecognized type,
    /// same as if it were written outside any generic context at all.
    pub fn resolve_spec_dependencies(
        &mut self,
        sp: &HirSpecDef,
    ) -> Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        sp.dependencies.iter().filter_map(|dep| self.resolve_spec_reference(sp.id, sp.span, dep)).collect()
    }

    /// Resolves one spec function's raw signature against `substitution`
    /// (`Self` plus the spec's own generics, bound to concrete types) --
    /// pushed as a temporary scope so this never disturbs whatever's
    /// already bound in the calling implementor's own `Context` (its own
    /// generics, already seeded when this `Analyzer` was constructed).
    fn resolve_raw_spec_fn_type(
        &mut self,
        id: HirId,
        span: Span,
        raw: &RawSpecFunctionSig,
        substitution: &[(Ident, ResolvedType)],
    ) -> Option<ResolvedFunctionType> {
        self.context.enter_scope();
        for (name, ty) in substitution {
            self.context.current_scope().defined_types.insert(name.clone(), ty.clone());
        }
        let mut params = Vec::with_capacity(raw.params.len());
        let mut ok = true;
        for p in &raw.params {
            match self.resolve_type_or_error(id, span, &p.r#type, true) {
                Some(r) => params.push((p.ident.clone(), r)),
                None => ok = false,
            }
        }
        let return_type = self.resolve_type_or_error(id, span, &raw.return_type, true);
        self.context.leave_scope();
        if !ok {
            return None;
        }
        Some(ResolvedFunctionType {
            params,
            return_type: Box::new(return_type?),
            is_variadic: false,
            is_member_function: raw.is_member_function,
        })
    }

    /// The full, ordered, deduplicated set of functions `spec<type_args>`
    /// requires from an implementor of type `self_type` -- walks
    /// `dependencies` depth-first (each dependency's own requirements
    /// appear before this spec's own, matching read-order intuition),
    /// substituting `Self -> self_type` and this spec's own generics ->
    /// `type_args` into every raw signature along the way. Two entries
    /// sharing a name must resolve to *structurally identical*
    /// `ResolvedFunctionType`s (point 5 of the user's design: "the type
    /// will only implement it once... the compiler may assume the same
    /// function for both") -- silently deduplicated when they match,
    /// `ConflictingSpecFunctions` when they don't. This one ordered list is
    /// also dynamic dispatch's vtable slot order (`Codegen`'s vtable
    /// builder walks it identically) -- see [[omega-enums-design]]/the
    /// spec design plan for why one flattening serves both purposes.
    fn flatten_spec(
        &mut self,
        id: HirId,
        span: Span,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        type_args: &[ResolvedType],
        self_type: &ResolvedType,
    ) -> Option<Vec<FlattenedSpecFn>> {
        let mut out = Vec::new();
        self.flatten_spec_into(id, span, spec, type_args, self_type, &mut out)?;
        Some(out)
    }

    fn flatten_spec_into(
        &mut self,
        id: HirId,
        span: Span,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        type_args: &[ResolvedType],
        self_type: &ResolvedType,
        out: &mut Vec<FlattenedSpecFn>,
    ) -> Option<()> {
        let (spec_name, generics, dependencies, functions) = {
            let s = spec.borrow();
            (s.name.clone(), s.generics.clone(), s.dependencies.clone(), s.functions.clone())
        };
        for (dep_spec, dep_args) in &dependencies {
            self.flatten_spec_into(id, span, dep_spec, dep_args, self_type, out)?;
        }

        let self_ident = Ident("Self".to_string());
        let substitution: Vec<(Ident, ResolvedType)> = std::iter::once((self_ident, self_type.clone()))
            .chain(generics.iter().cloned().zip(type_args.iter().cloned()))
            .collect();

        for (name, raw) in &functions {
            let fn_type = self.resolve_raw_spec_fn_type(id, span, raw, &substitution)?;
            if let Some(existing_index) = out.iter().position(|f| &f.name == name) {
                let existing = &out[existing_index];
                if existing.fn_type != fn_type {
                    self.errors.push(AnalysisError::new(
                        id,
                        span,
                        AnalysisErrorKind::ConflictingSpecFunctions {
                            name: name.clone(),
                            first_spec: existing.spec_name.clone(),
                            second_spec: spec_name.clone(),
                        },
                    ));
                    return None;
                }
                // Same signature, already present -- ordinarily a silent
                // dedup (point 5 of the language design: one implementation
                // serves every spec that required it). But if the earlier
                // occurrence came from a bare *requirement* (no default
                // body -- typically a dependency, like `Dummy`'s own
                // `dummy`) and this one provides an actual default (a
                // dependent spec satisfying its own dependency, like
                // `Mammal`'s `dummy`), this later, more-specific default
                // must win -- an implementor should never be asked for a
                // function its own declared spec already gave it a body
                // for, just because that spec happened to flatten a bare
                // requirement first.
                if existing.raw.default_body.is_none() && raw.default_body.is_some() {
                    out[existing_index] = FlattenedSpecFn {
                        name: name.clone(),
                        fn_type,
                        raw: raw.clone(),
                        spec_name: spec_name.clone(),
                        substitution: substitution.clone(),
                    };
                }
                continue;
            }
            out.push(FlattenedSpecFn {
                name: name.clone(),
                fn_type,
                raw: raw.clone(),
                spec_name: spec_name.clone(),
                substitution: substitution.clone(),
            });
        }
        Some(())
    }

    /// Resolves a struct/enum/union's `implements` clause: flattens every
    /// declared spec (dependencies included, cross-entry dedup/conflict
    /// handled the same way `flatten_spec_into` already handles it within
    /// one spec -- everything accumulates into one shared list), then for
    /// each required function not already provided by `own_functions`,
    /// either queues a default-method instantiation (spec supplied a body)
    /// or reports `MissingSpecFunction`. An own method whose *name*
    /// matches but whose signature doesn't is treated the same as missing
    /// -- it doesn't actually satisfy the contract. Returns the additional
    /// `(name, ResolvedMethod)` entries to merge into the implementor's own
    /// `functions` list (already carrying freshly minted `decl_id`s) plus
    /// every queued default body still needing to be checked in phase 2.
    fn resolve_implements_clause(
        &mut self,
        id: HirId,
        span: Span,
        implementor_name: &Ident,
        implements: &[Type],
        own_functions: &[(Ident, ResolvedMethod)],
        self_type: &ResolvedType,
    ) -> (Vec<(Ident, ResolvedMethod)>, Vec<PendingSpecMethod>) {
        let mut flattened: Vec<FlattenedSpecFn> = Vec::new();
        for spec_type in implements {
            let Some((spec, type_args)) = self.resolve_spec_reference(id, span, spec_type) else { continue };
            // A conflict within this flattening is already reported inline
            // (`ConflictingSpecFunctions`) -- nothing further to do here on
            // `None` besides skipping this entry's remaining contribution.
            let _ = self.flatten_spec_into(id, span, &spec, &type_args, self_type, &mut flattened);
        }

        let mut extra_methods = Vec::new();
        let mut pending = Vec::new();
        for req in flattened {
            if let Some((_, own)) = own_functions.iter().find(|(name, _)| *name == req.name) {
                if own.fn_type != req.fn_type {
                    self.errors.push(AnalysisError::new(
                        id,
                        span,
                        AnalysisErrorKind::MissingSpecFunction {
                            implementor: implementor_name.clone(),
                            spec: req.spec_name.clone(),
                            function: req.name.clone(),
                        },
                    ));
                }
                continue;
            }
            match &req.raw.default_body {
                Some(_) => {
                    let minted_id = self.resolver.fresh_synthetic_id();
                    extra_methods
                        .push((req.name.clone(), ResolvedMethod { decl_id: minted_id, fn_type: req.fn_type.clone() }));
                    pending.push(PendingSpecMethod {
                        id: minted_id,
                        fn_type: req.fn_type,
                        raw: req.raw,
                        substitution: req.substitution,
                    });
                }
                None => {
                    self.errors.push(AnalysisError::new(
                        id,
                        span,
                        AnalysisErrorKind::MissingSpecFunction {
                            implementor: implementor_name.clone(),
                            spec: req.spec_name.clone(),
                            function: req.name.clone(),
                        },
                    ));
                }
            }
        }
        (extra_methods, pending)
    }

    /// Whether `ty` (an already-concrete, resolved type) implements
    /// `spec<spec_type_args>` -- flattens the spec's requirements with
    /// `Self = ty` and checks each one is actually present, by name and
    /// exact signature, in `ty`'s own method list (`find_methods`). By the
    /// time this ever runs, any type that genuinely implements a spec
    /// already has every required function merged into its own list (own
    /// override or spec-default instantiation -- see
    /// `resolve_implements_clause`); this never re-derives that, only
    /// confirms it. In practice this only ever fires for a primitive type
    /// (which has no method list at all -- spec implementation is scoped
    /// to struct/enum/union) or a genuine caller mistake (a spec-bound
    /// generic instantiated with a type that never declared `: Spec` at
    /// all). Returns the missing function names on failure.
    fn type_implements_spec(
        &mut self,
        id: HirId,
        span: Span,
        ty: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_type_args: &[ResolvedType],
    ) -> Result<(), Vec<Ident>> {
        let Some(required) = self.flatten_spec(id, span, spec, spec_type_args, ty) else {
            return Err(vec![]);
        };
        let missing: Vec<Ident> = required
            .iter()
            .filter(|req| !self.find_methods(ty, &req.name).iter().any(|m| m.fn_type == req.fn_type))
            .map(|req| req.name.clone())
            .collect();
        if missing.is_empty() { Ok(()) } else { Err(missing) }
    }

    /// Checks a single generic bound (`T: Animal`) against the concrete
    /// type `T` was instantiated with -- the public entry point
    /// `omega_driver::Driver::ensure_item`'s bound-checking uses (spec
    /// resolution/flattening themselves stay private implementation
    /// details). `None` when `bound` itself failed to resolve at all
    /// (already recorded as an ordinary `AnalysisError`, folded into
    /// `self.errors`/`finish()` as usual) -- distinguished from
    /// `Some(Err(..))` (`bound` resolved fine, `concrete` just doesn't
    /// satisfy it) so the caller can tell "my own error already reported"
    /// apart from a real, reportable `SpecNotImplemented`.
    pub fn check_generic_bound(
        &mut self,
        id: HirId,
        span: Span,
        bound: &Type,
        concrete: &ResolvedType,
    ) -> Option<Result<(), (Ident, Vec<Ident>)>> {
        let (spec, spec_args) = self.resolve_spec_reference(id, span, bound)?;
        let spec_name = spec.borrow().name.clone();
        match self.type_implements_spec(id, span, concrete, &spec, &spec_args) {
            Ok(()) => Some(Ok(())),
            Err(missing) => Some(Err((spec_name, missing))),
        }
    }

    /// Wraps `value` in `CheckedExpr::SpecCoerce` when `expected` is a
    /// `SpecObject` and `value`'s own type is a plain pointer to a struct/
    /// enum/union that implements the target spec -- see
    /// `CheckedExpr::SpecCoerce`'s doc comment for why this needs an
    /// explicit node rather than being folded into `ResolvedType::accepts`
    /// itself. A no-op (returns `value` unchanged) whenever no such
    /// coercion applies -- including when `expected` already structurally
    /// `accepts` `value`, or when the spec isn't actually implemented (in
    /// which case the caller's own ordinary `accepts` check reports the
    /// mismatch exactly as before, just without this specific "why" -- an
    /// accepted simplification, not every coercion site needs its own
    /// bespoke diagnostic).
    fn coerce_to_expected(&mut self, expected: Option<&ResolvedType>, value: CheckedExprNode) -> CheckedExprNode {
        let Some(target @ ResolvedType::SpecObject { spec, type_args, mutable: expected_mutable }) = expected else {
            return value;
        };
        if target.accepts(&value.r#type) {
            return value;
        }
        let ResolvedType::Pointer { pointee, mutable: value_mutable } = &value.r#type else { return value };
        if !*value_mutable && *expected_mutable {
            return value;
        }
        if self.type_implements_spec(value.id, value.span, pointee, spec, type_args).is_err() {
            return value;
        }
        CheckedExprNode {
            id: value.id,
            span: value.span,
            r#type: target.clone(),
            kind: CheckedExpr::SpecCoerce(CheckedSpecCoerce { base: Box::new(value) }),
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
        r#type: ResolvedType,
        storage: Storage,
        mutable: bool,
    ) -> Option<()> {
        self.declare_binding_impl(id, span, ident, r#type, storage, false, mutable)
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
        r#type: ResolvedType,
        storage: Storage,
        mutable: bool,
    ) -> Option<()> {
        self.declare_binding_impl(id, span, ident, r#type, storage, true, mutable)
    }

    fn declare_binding_impl(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        r#type: ResolvedType,
        storage: Storage,
        narrowed: bool,
        mutable: bool,
    ) -> Option<()> {
        let binding = VarBinding { decl_id: id, storage, r#type, span, narrowed, mutable };
        match self.context.current_scope().declare(ident.clone(), binding) {
            Ok(()) => Some(()),
            Err((name, previous)) => {
                self.errors.push(AnalysisError::new(
                    id,
                    span,
                    AnalysisErrorKind::Redeclaration { name, previous: Some(previous) },
                ));
                None
            }
        }
    }

    pub fn analyze_declaration(&mut self, decl: &HirDeclaration, storage: Storage) -> Option<CheckedDeclaration> {
        // A global's type is never itself embedded inline into another
        // type's layout (it isn't a struct field), so it can never be part
        // of an infinite-size cycle -- always indirect.
        let resolved_type = self.resolve_type_or_error(decl.id, decl.span, &decl.r#type, true)?;
        self.declare_binding(decl.id, decl.span, &decl.ident, resolved_type.clone(), storage, decl.mutable)?;
        Some(CheckedDeclaration {
            id: decl.id,
            span: decl.span,
            ident: decl.ident.clone(),
            r#type: resolved_type,
        })
    }

    pub fn analyze_extern_decl(&mut self, extern_decl: &HirExternDeclaration) -> Option<CheckedExternDeclaration> {
        let resolved_type = self.resolve_type_or_error(extern_decl.id, extern_decl.span, &extern_decl.r#type, true)?;
        // An extern of function type imports a callable symbol; anything
        // else is extern *data*, whose storage isn't decided yet (see
        // `Storage::Global`'s doc comment).
        let storage = if matches!(resolved_type, ResolvedType::Function(_)) {
            Storage::Function
        } else {
            Storage::Global
        };
        // `extern` declarations are always immutable for now -- no existing
        // use case needs mutable extern data, and `mut extern` can be added
        // later without breaking anything (see `omega_parser`'s `mut`
        // contextual-keyword sites, none of which check for it here).
        self.declare_binding(
            extern_decl.id,
            extern_decl.span,
            &extern_decl.ident,
            resolved_type.clone(),
            storage,
            false,
        )?;
        Some(CheckedExternDeclaration {
            id: extern_decl.id,
            span: extern_decl.span,
            ident: extern_decl.ident.clone(),
            r#type: resolved_type,
        })
    }

    fn analyze_param(&mut self, param: &HirParam) -> Option<CheckedParam> {
        // A parameter is passed by value at the call site, not laid out
        // inline inside anything -- a method taking its own struct type by
        // value (`fn combine(self, other: Self) -> Self`) is completely
        // ordinary and must not be flagged as a layout cycle.
        let resolved_type = self.resolve_type_or_error(param.id, param.span, &param.r#type, true)?;
        // Parameters (including `self`) are always immutable bindings --
        // `mut` is never recognized in parameter position at all (see
        // `omega_parser::parser::item::parse_declaration_list`); a
        // parameter that needs to vary locally can be shadowed
        // (`mut x := param;`). `self`'s own *pointee* mutability (`mut
        // self` vs `self`) is a separate, `ResolvedType::Pointer` concern,
        // already baked into `resolved_type` here.
        self.declare_binding(param.id, param.span, &param.ident, resolved_type.clone(), Storage::Parameter, false)?;
        Some(CheckedParam {
            id: param.id,
            span: param.span,
            ident: param.ident.clone(),
            r#type: resolved_type,
        })
    }

    /// Struct fields aren't scope-bound names (they're only ever reached
    /// through a `FieldAccess` projection off a struct-typed base), so unlike
    /// params they don't go through `declare_binding` -- but duplicate field
    /// names are still rejected, via a plain per-struct name set.
    fn analyze_struct_fields(&mut self, fields: &[HirParam]) -> Option<Vec<CheckedParam>> {
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        self.analyze_all(fields, |this, field| {
            if let Some(previous) = seen.insert(field.ident.clone(), field.span) {
                this.errors.push(AnalysisError::new(
                    field.id,
                    field.span,
                    AnalysisErrorKind::Redeclaration { name: field.ident.clone(), previous: Some(previous) },
                ));
                return None;
            }
            // A field is the one context that genuinely lays its type out
            // inline -- this is the case `RecursiveTypeWithoutIndirection`
            // exists to catch, so it's the only caller passing `false`.
            let resolved_type = this.resolve_type_or_error(field.id, field.span, &field.r#type, false)?;
            Some(CheckedParam {
                id: field.id,
                span: field.span,
                ident: field.ident.clone(),
                r#type: resolved_type,
            })
        })
    }

    /// Resolves a single `.field` step against `current_type`, inserting a
    /// seamless one-level pointer deref first if needed (`ptr.field` is
    /// sugar for `(*ptr).field` when `ptr` is a pointer-to-struct, matching
    /// Rust's autoderef -- exactly one level: `ptr.field` where `ptr` is
    /// `**Struct` still needs an explicit `(*ptr).field`). Shared by
    /// `analyze_place`'s projection loop and member-call resolution below,
    /// so both plain field access and method access get this for free from
    /// one implementation. `mutable`, like `analyze_place`'s own running
    /// mutability, is overwritten with the pointer's own flag when this
    /// inserts a seamless deref -- callers that don't care about mutability
    /// (a read, or a callable-field lookup) just pass a throwaway `&mut
    /// bool`.
    fn resolve_field_projection(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        current_type: &ResolvedType,
        field: &Ident,
        mutable: &mut bool,
    ) -> Option<ResolvedType> {
        let dereffed = match current_type {
            ResolvedType::Pointer { pointee, mutable: pointer_mutable } => {
                *mutable = *pointer_mutable;
                projections.push(CheckedProjection::Deref { r#type: (**pointee).clone() });
                (**pointee).clone()
            }
            other => other.clone(),
        };

        // `slice.length` -- not a real field (a slice isn't a `Struct`), so
        // this is checked before the struct-only path below rejects it. Any
        // other field name on a slice is simply `NoSuchField`, same message a
        // struct without that field would give.
        if let ResolvedType::Slice { .. } = &dereffed {
            if field.as_ref() == "length" {
                projections.push(CheckedProjection::SliceLength);
                return Some(ResolvedType::I32);
            }
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::NoSuchField { field: field.clone(), base: dereffed.clone() },
            ));
            return None;
        }

        // Enum member access: `tag`, header fields, and shared dynamic
        // fields exist on every value; a body field additionally requires
        // the value's variant to be statically known (see `ResolvedType::
        // Enum`) *and* to be the one declaring it -- anything else gets
        // the most precise "why not" this lookup can determine.
        if let ResolvedType::Enum { cell, variant } = &dereffed {
            let e = cell.borrow();
            if field.as_ref() == "tag" {
                let r#type = e.tag_type.clone();
                projections.push(CheckedProjection::EnumTag { r#type: r#type.clone() });
                return Some(r#type);
            }
            if let Some((index, (_, r#type))) = e.header.iter().enumerate().find(|(_, (name, _))| name == field) {
                let r#type = r#type.clone();
                projections.push(CheckedProjection::EnumHeader { field: field.clone(), index, r#type: r#type.clone() });
                return Some(r#type);
            }
            if let Some((index, (_, r#type))) =
                e.dynamic_fields.iter().enumerate().find(|(_, (name, _))| name == field)
            {
                let r#type = r#type.clone();
                projections.push(CheckedProjection::EnumDynamicField {
                    field: field.clone(),
                    index,
                    r#type: r#type.clone(),
                });
                return Some(r#type);
            }
            if let Some(current) = variant
                && let Some((field_index, (_, r#type))) =
                    e.variants[*current].fields.iter().enumerate().find(|(_, (name, _))| name == field)
            {
                let r#type = r#type.clone();
                projections.push(CheckedProjection::EnumBody {
                    variant_index: *current,
                    field_index,
                    r#type: r#type.clone(),
                });
                return Some(r#type);
            }
            let owner = e.variants.iter().find(|v| v.fields.iter().any(|(name, _)| name == field));
            let kind = match (owner, variant) {
                (Some(owner), Some(current)) => AnalysisErrorKind::EnumFieldWrongVariant {
                    field: field.clone(),
                    owner: owner.name.clone(),
                    actual: e.variants[*current].name.clone(),
                },
                (Some(owner), None) => AnalysisErrorKind::EnumFieldVariantUnknown {
                    field: field.clone(),
                    r#enum: e.name.clone(),
                    owner: owner.name.clone(),
                },
                (None, _) => {
                    // Suggest across everything reachable as `value.name` on
                    // this value: tag, header, shared dynamic fields, and --
                    // when the variant is known -- its own body fields.
                    let tag = Ident("tag".into());
                    let candidates = std::iter::once(&tag)
                        .chain(e.header.iter().map(|(name, _)| name))
                        .chain(e.dynamic_fields.iter().map(|(name, _)| name))
                        .chain(
                            variant
                                .iter()
                                .flat_map(|&i| e.variants[i].fields.iter().map(|(name, _)| name)),
                        );
                    AnalysisErrorKind::NoSuchEnumField {
                        field: field.clone(),
                        r#enum: e.name.clone(),
                        similar: best_match(field, candidates),
                    }
                }
            };
            drop(e);
            self.errors.push(AnalysisError::new(node_id, span, kind));
            return None;
        }

        if let ResolvedType::Union(union_type) = &dereffed {
            let union_type = union_type.borrow();
            let found = union_type
                .fields
                .iter()
                .enumerate()
                .find(|(_, (name, _))| name == field)
                .map(|(index, (_, r#type))| (index, r#type.clone()));
            let Some((index, field_type)) = found else {
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::NoSuchField { field: field.clone(), base: dereffed.clone() },
                ));
                return None;
            };
            projections.push(CheckedProjection::UnionField {
                field: field.clone(),
                index,
                r#type: field_type.clone(),
            });
            return Some(field_type);
        }

        let ResolvedType::Struct(struct_type) = &dereffed else {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAStruct { found: dereffed.clone() }));
            return None;
        };
        let struct_type = struct_type.borrow();

        let found = struct_type
            .fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == field)
            .map(|(index, (_, r#type))| (index, r#type.clone()));
        let Some((index, field_type)) = found else {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::NoSuchField { field: field.clone(), base: dereffed.clone() },
            ));
            return None;
        };

        projections.push(CheckedProjection::FieldAccess {
            field: field.clone(),
            index,
            r#type: field_type.clone(),
        });
        Some(field_type)
    }

    /// Read-only peek at whether `field`, applied to `current_type` (after
    /// the same up-to-one-level seamless deref `resolve_field_projection`
    /// would apply), names a struct method rather than a field -- used to
    /// detect a member call (`base.method(args)`) before committing to
    /// resolving `field` as an ordinary field access. A field with this name
    /// always shadows a method with the same name.
    /// Every method named `field` on `current_type` (after at most one
    /// pointer deref) -- usually zero or one, but two or more is a valid
    /// overload set (see `Analyzer::resolve_overload`, which the two call
    /// sites route a multi-candidate result through). A field with this
    /// name always shadows every same-named method, exactly like a single
    /// method would have.
    fn find_methods(&self, current_type: &ResolvedType, field: &Ident) -> Vec<ResolvedMethod> {
        let dereffed = match current_type {
            ResolvedType::Pointer { pointee, .. } => pointee.as_ref(),
            other => other,
        };
        match dereffed {
            ResolvedType::Struct(struct_type) => {
                let struct_type = struct_type.borrow();
                if struct_type.fields.iter().any(|(name, _)| name == field) {
                    return Vec::new();
                }
                struct_type
                    .functions
                    .iter()
                    .filter(|(name, _)| name == field)
                    .map(|(_, method)| method.clone())
                    .collect()
            }
            ResolvedType::Enum { cell, variant } => {
                // Anything reachable as a field on *this* value (`tag`,
                // header, the known variant's body fields) shadows a
                // same-named function, matching the struct rule above.
                let e = cell.borrow();
                let shadowed = field.as_ref() == "tag"
                    || e.header.iter().any(|(name, _)| name == field)
                    || variant.is_some_and(|i| e.variants[i].fields.iter().any(|(name, _)| name == field));
                if shadowed {
                    return Vec::new();
                }
                e.functions.iter().filter(|(name, _)| name == field).map(|(_, method)| method.clone()).collect()
            }
            ResolvedType::Union(union_type) => {
                let union_type = union_type.borrow();
                if union_type.fields.iter().any(|(name, _)| name == field) {
                    return Vec::new();
                }
                union_type
                    .functions
                    .iter()
                    .filter(|(name, _)| name == field)
                    .map(|(_, method)| method.clone())
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The name of an enum member that an assignment must not target --
    /// `Some` when `target`'s final projection reads the tag or a header
    /// field (both per-variant constants); see the `Assignment` arm of
    /// `analyze_expr`.
    fn immutable_enum_member(target: &CheckedPlace) -> Option<Ident> {
        match target.projections.last()? {
            CheckedProjection::EnumTag { .. } => Some(Ident("tag".into())),
            CheckedProjection::EnumHeader { field, .. } => Some(field.clone()),
            _ => None,
        }
    }

    /// Errors (returning `None`) unless a place `analyze_place` already
    /// resolved (`mutable` is its own third return value) may be written
    /// to. Shared by every requirement that ultimately means the same
    /// thing -- an assignment, `++`/`--`, an explicit `&mut`, and a `mut
    /// self` method call's implicit auto-ref are all, at bottom, "this
    /// place must be mutable" -- so the diagnostic (and the choice between
    /// `NotMutableBinding`/`NotMutablePointer`, mirroring
    /// `immutable_enum_member`'s pattern of inspecting the checked place's
    /// own projections) only needs writing once. `hir_root` is the
    /// *original* place's root, for naming the binding in
    /// `NotMutableBinding` -- only ever `None` when the reason is
    /// definitely `NotMutablePointer` instead (a non-place root, e.g. a
    /// freshly-constructed value, is never itself the *cause* of
    /// immutability -- something dereferenced along the way always is).
    fn require_mutable_place(
        &mut self,
        node_id: HirId,
        span: Span,
        hir_root: &HirPlaceRoot,
        checked_place: &CheckedPlace,
        mutable: bool,
    ) -> Option<()> {
        if mutable {
            return Some(());
        }
        let through_pointer = checked_place.projections.iter().any(|p| matches!(p, CheckedProjection::Deref { .. }));
        let kind = if through_pointer {
            AnalysisErrorKind::NotMutablePointer
        } else {
            match hir_root {
                HirPlaceRoot::Path(p) if p.path.is_unqualified() => {
                    AnalysisErrorKind::NotMutableBinding { ident: p.path.head.clone() }
                }
                _ => AnalysisErrorKind::NotMutablePointer,
            }
        };
        self.errors.push(AnalysisError::new(node_id, span, kind));
        None
    }

    /// `&base[range]` (`requested_mutable: false`) / `&mut base[range]`
    /// (`requested_mutable: true`) -- the only way to produce a
    /// `ResolvedType::Slice` value; a bare `base[range]` with no `&`/`&mut`
    /// is rejected before this is ever called (see `HirExpr::Slice`'s arm
    /// in `analyze_expr`). Mirrors `HirExpr::AddressOf`'s own `&`/`&mut`
    /// treatment of an ordinary place, just producing a fat pointer instead
    /// of a thin one.
    fn analyze_slice(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirPlace,
        range: &HirRange,
        requested_mutable: bool,
    ) -> Option<CheckedExprNode> {
        let (checked_base, base_type, place_mutable) = self.analyze_place(node_id, span, base, None)?;

        // The slice's *source* mutability: for inline storage
        // (`SizedArray`), whether the storage being sliced is itself
        // writable (the place's own mutability); for re-slicing an
        // existing `Slice` value, that slice's own flag -- a property of
        // the value being sliced, independent of whether the *variable*
        // holding it happens to be `mut`. `requested_mutable` (from the
        // `&`/`&mut` the user actually wrote) may never exceed this: you
        // can't get a mutable slice out of an immutable array or an
        // already-immutable slice.
        let (item_type, source_mutable, from_slice) = match base_type {
            ResolvedType::SizedArray(item_type, _) => (*item_type, place_mutable, false),
            ResolvedType::Slice { item, mutable } => (*item, mutable, true),
            found => {
                self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotSliceable { found }));
                return None;
            }
        };
        if requested_mutable && !source_mutable {
            // Re-slicing an already-immutable `Slice` value: `require_
            // mutable_place` below would blame the *binding* (`&base.root`),
            // which is misleading here -- the binding may well be `mut`,
            // it's the slice *value* it holds that's immutable (see the
            // comment above). Only the plain-array case is a genuine
            // binding-mutability question.
            if from_slice {
                self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::ImmutableSliceSource));
                return None;
            }
            self.require_mutable_place(node_id, span, &base.root, &checked_base, source_mutable)?;
        }

        let analyze_bound = |this: &mut Self, bound: &Option<Box<HirExprNode>>| -> Option<Option<Box<CheckedExprNode>>> {
            let Some(bound) = bound else { return Some(None) };
            let checked_bound = this.analyze_expr(bound, Some(&ResolvedType::I32))?;
            if checked_bound.r#type != ResolvedType::I32 {
                this.errors.push(AnalysisError::new(
                    bound.id,
                    bound.span,
                    AnalysisErrorKind::InvalidSliceBound { r#type: checked_bound.r#type },
                ));
                return None;
            }
            Some(Some(Box::new(checked_bound)))
        };

        let checked_start = analyze_bound(self, &range.start)?;
        let checked_end = analyze_bound(self, &range.end)?;

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Slice { item: Box::new(item_type.clone()), mutable: requested_mutable },
            kind: CheckedExpr::Slice(CheckedSlice {
                base: checked_base,
                item_type,
                start: checked_start,
                end: checked_end,
                inclusive: range.inclusive,
            }),
        })
    }

    /// `&[...]` (compile-time slice literals are always immutable --
    /// `&mut [...]` is rejected here, unlike `analyze_slice`, which has a
    /// real mutable form). The element type comes from a declared/expected
    /// `Slice` type if one is in context (e.g. `x: *[i32] = &[1, 2, 3];`),
    /// otherwise from the first element's own ordinary-expression type
    /// (reusing `analyze_expr`'s existing literal-default inference, e.g.
    /// an unsuffixed number defaults to `i32`, rather than reinventing it)
    /// -- exactly the same two-source shape the ordinary `HirExpr::
    /// ArrayLiteral` arm above already uses. Every element is then
    /// re-evaluated as a compile-time constant via `const_eval_slice`, and
    /// the whole literal collapses to one `ConstValue::Slice`, baked into
    /// the binary's data segment at codegen (`Codegen::emit_const_slice`)
    /// rather than built on the stack.
    fn analyze_const_slice(
        &mut self,
        node_id: HirId,
        span: Span,
        elements: &[HirExprNode],
        mutable: bool,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        if mutable {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ConstSliceCannotBeMutable));
            return None;
        }
        if elements.is_empty() {
            self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::EmptyArrayLiteral));
            return None;
        }

        let item_type = match expected {
            Some(ResolvedType::Slice { item, mutable: false }) => item.as_ref().clone(),
            _ => self.analyze_expr(&elements[0], None)?.r#type.widened(),
        };

        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.const_eval_slice(element, &item_type)?);
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Slice { item: Box::new(item_type), mutable: false },
            kind: CheckedExpr::ConstSlice(ConstValue::Slice(values)),
        })
    }

    /// `analyze_const_slice`'s per-element evaluator -- the `&[...]`
    /// sibling of `const_eval`, kept deliberately separate for the exact
    /// reason `const_eval_pattern` is: `const_eval`'s fallback errors
    /// (`EnumValueNotConstant`/`EnumValueTypeMismatch`) are worded
    /// specifically for enum header values, which would be a confusing
    /// thing to say about `a := &[1, f()];`. The actual literal
    /// recognition (including its own recursive `AddressOf`-wrapped-
    /// `ArrayLiteral` case, for nested compile-time slices -- a *bare*
    /// nested array is never recognized, even here, to keep `&[...]` the
    /// one unambiguous spelling) is otherwise identical, and both share
    /// `const_number` for the real parsing/range-checking work.
    fn const_eval_slice(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        let mismatch = |this: &mut Self, found: &str| {
            this.errors.push(AnalysisError::new(
                expr.id,
                expr.span,
                AnalysisErrorKind::ConstSliceElementTypeMismatch { expected: expected.clone(), found: found.into() },
            ));
            None
        };
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => {
                    self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number)
                }
                _ => {
                    self.errors
                        .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::ConstSliceElementNotConstant));
                    None
                }
            },
            HirExpr::String(s) => match expected {
                ResolvedType::Pointer { pointee, mutable: false } if **pointee == ResolvedType::U8 => {
                    Some(ConstValue::Str(s.0.clone()))
                }
                _ => mismatch(self, "a string literal"),
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => mismatch(self, "a bool literal"),
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => mismatch(self, "a character literal"),
            },
            // A bare `[...]` -- a fixed-length array element (e.g. a slice
            // of fixed-size arrays, `*[[i32; 2]]`) has no indirection of its
            // own, so it's written the same way it would be as a bare enum
            // header value: no `&`, matching `const_eval`'s identical case.
            HirExpr::ArrayLiteral(elements) => match expected {
                ResolvedType::SizedArray(item, size) => {
                    if elements.len() != *size as usize {
                        return mismatch(self, &format!("an array literal with {} elements", elements.len()));
                    }
                    let mut values = Vec::with_capacity(elements.len());
                    for element in elements {
                        values.push(self.const_eval_slice(element, item)?);
                    }
                    Some(ConstValue::Array(values))
                }
                _ => mismatch(self, "an array literal"),
            },
            // `&[...]` is the only recognized spelling for a nested
            // compile-time slice -- a bare `[...]` is never treated as one,
            // even here, to avoid confusing it with an ordinary array.
            // `&mut [...]` still isn't allowed, even nested.
            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                if *mutable {
                    self.errors
                        .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::ConstSliceCannotBeMutable));
                    return None;
                }
                match &base.expr {
                    HirExpr::ArrayLiteral(nested) => match expected {
                        ResolvedType::Slice { item, mutable: false } => {
                            let mut values = Vec::with_capacity(nested.len());
                            for element in nested {
                                values.push(self.const_eval_slice(element, item)?);
                            }
                            Some(ConstValue::Slice(values))
                        }
                        _ => mismatch(self, "an array literal"),
                    },
                    _ => {
                        self.errors.push(AnalysisError::new(
                            expr.id,
                            expr.span,
                            AnalysisErrorKind::ConstSliceElementNotConstant,
                        ));
                        None
                    }
                }
            }
            _ => {
                self.errors
                    .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::ConstSliceElementNotConstant));
                None
            }
        }
    }

    /// What `alias` means as an import in this module, resolved lazily and
    /// memoized by the driver per `(module_path, alias)` pair -- `Ok(None)`
    /// means this module has no `import` statement binding `alias` at all,
    /// the signal every caller's own "assume this is my own module's item"
    /// fallback keys off. This is the direct replacement for the old
    /// `Context::absolute_path`/`generic_alias`/`bind_imported_item`, which
    /// used to be populated eagerly, for a module's *entire* import list,
    /// before any item in it was ever touched -- see `Analyzer::new`'s doc
    /// comment for why that was a real false-cycle bug, not just eagerness.
    fn resolve_alias(&mut self, alias: &Ident) -> Result<Option<ImportTarget>, ResolveError> {
        self.resolver.resolve_import_alias(&self.module_path, alias)
    }

    /// `resolve_alias`, with a real resolution failure (a cycle, a broken
    /// target module, ...) folded directly into `self.errors` -- the
    /// `Option<Option<_>>` "handled or fall through" shape every *hard*
    /// (non-probing) call site wants: outer `None` means an error was
    /// already pushed and the caller should give up immediately (`?`);
    /// `Some(None)` means `alias` isn't an import at all, the caller's own
    /// fallback applies.
    fn resolve_alias_or_error(&mut self, node_id: HirId, span: Span, alias: &Ident) -> Option<Option<ImportTarget>> {
        match self.resolve_alias(alias) {
            Ok(target) => Some(target),
            Err(e) => {
                self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                None
            }
        }
    }

    /// The alias (of any kind -- module, item, or generic item) this
    /// module's own `import` statements bind that's most similar to
    /// `target` -- the "did you mean" suggestion for a reference that named
    /// nothing at all. Replaces `Context`'s old `similar_module_alias`
    /// (which only ever knew about whole-module aliases, pre-populated
    /// eagerly); `ModuleResolver::import_alias_names` is the only remaining
    /// place that knows a module's whole alias set up front, since
    /// resolving what each one actually *means* is lazy now.
    fn similar_import_alias(&mut self, target: &Ident) -> Option<Ident> {
        best_match(target, self.resolver.import_alias_names(&self.module_path).iter())
    }

    /// Resolves `absolute` (already a full `[module_path.., name]`, whether
    /// built from a qualified place's import alias or an unqualified one's
    /// implicit own-module prefix) to a place root -- shared by both of
    /// `analyze_place`'s non-local cases so the `Value`/`Type`/`Err` match
    /// is only written once.
    /// `unqualified` is the bare name the user actually wrote, when this
    /// query is the implicit own-module fallback for one -- an
    /// `UnknownItem` miss then means "no such variable", and is reported as
    /// exactly that (with a typo suggestion from the visible scopes) rather
    /// than as a confusing module-shaped error about the module the user
    /// never mentioned.
    fn resolve_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: Vec<Ident>,
        unqualified: Option<&Ident>,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        // A bare (uncalled) reference to an overloaded name -- `resolve_item`
        // would otherwise silently resolve it to whichever candidate the
        // driver happens to index first (see `ModuleResolver::resolve_item`'s
        // single-result contract, which has no way to pick one at all). A
        // call site (`resolve_overloaded_call`) has the argument types
        // needed to disambiguate; anywhere else, the only other thing that
        // can disambiguate is an explicit function-typed `expected` (a
        // declaration/assignment annotation) that structurally matches
        // exactly one candidate's signature -- everything else is
        // unconditionally ambiguous, reported with every candidate listed
        // and no winner.
        if let Some((name, module_path)) = absolute.split_last()
            && let Ok(Some(candidates)) = self.resolver.function_overload_signatures(module_path, name)
        {
            if let Some(ResolvedType::Function(expected_fn)) = expected
                && let Some((decl_id, fn_type)) = Self::unique_overload_signature_match(expected_fn, &candidates)
            {
                let r#type = ResolvedType::Function(fn_type);
                let root = CheckedPlaceRoot::Variable { decl_id, storage: Storage::Function, r#type: r#type.clone() };
                return Some((root, r#type));
            }
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: candidates.into_iter().map(|(_, t)| t).collect(),
                },
            ));
            return None;
        }
        match self.resolver.resolve_item(&absolute, &[], true) {
            Ok(ResolvedItem::Value { r#type, storage, decl_id }) => {
                let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                Some((root, r#type))
            }
            Ok(ResolvedItem::Type(_)) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAValue(absolute)));
                None
            }
            Err(ResolveError::UnknownItem { .. }) if unqualified.is_some() => {
                let name = unqualified.expect("checked by the guard").clone();
                // Scope-level candidates first, then this module's own
                // top-level values (functions/globals/externs) -- only the
                // resolver holds a module-wide name list.
                let similar = self.context.similar_variable_name(&name).or_else(|| {
                    self.resolver.similar_item_name(&self.module_path, &name, ItemNamespace::Value)
                });
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::UndefinedVariable { name, similar }));
                None
            }
            // `mymodule::MyStruct::do_thing` -- the "module" that failed to
            // resolve (`mymodule::MyStruct`) may actually be a struct, and
            // the last segment one of its static functions. Only attempted
            // when the missing module is exactly this path minus its last
            // segment (a deeper miss can't be this shape).
            Err(ResolveError::UnknownModule(missing))
                if missing.len() + 1 == absolute.len() && missing == absolute[..missing.len()] =>
            {
                match self.resolver.resolve_item(&missing, &[], true) {
                    Ok(ResolvedItem::Type(t)) => {
                        self.resolve_type_member(node_id, span, &t, &absolute[missing.len()..])
                    }
                    _ => {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            span,
                            AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(missing)),
                        ));
                        None
                    }
                }
            }
            Err(e) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                None
            }
        }
    }

    /// Finds the one candidate (if any) among an overloaded name's
    /// signatures that structurally matches `expected` -- a function-typed
    /// declaration/assignment annotation naming exactly which overload is
    /// meant (`f : (a: u64) => void = f;`). Compared by shape only (param
    /// types in order, return type, `is_variadic`/`is_member_function`),
    /// never by parameter name -- the annotation's own parameter names have
    /// no reason to match the target function's, same "types only" spirit
    /// as `check_overload_duplicates`'s pairwise comparison. Zero or 2+
    /// matches both return `None`: a real duplicate overload set is already
    /// rejected elsewhere (`check_overload_duplicates`), so 2+ here would
    /// mean the annotation itself is ambiguous, not that a choice exists.
    fn unique_overload_signature_match(
        expected: &ResolvedFunctionType,
        candidates: &[(HirId, ResolvedFunctionType)],
    ) -> Option<(HirId, ResolvedFunctionType)> {
        let mut matches = candidates.iter().filter(|(_, fn_type)| {
            fn_type.is_variadic == expected.is_variadic
                && fn_type.is_member_function == expected.is_member_function
                && fn_type.return_type == expected.return_type
                && fn_type.params.len() == expected.params.len()
                && fn_type.params.iter().zip(&expected.params).all(|((_, a), (_, b))| a == b)
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.clone())
    }

    /// `Head::function` where `Head` isn't an imported module alias -- the
    /// head may instead name a struct *type* (a builtin/imported/locally
    /// defined one via `find_defined_type`, or this module's own top-level
    /// struct via the resolver), making this a static-function reference.
    /// Reports the most precise error it can when the head names nothing
    /// usable -- `ModuleNotImported` only when the head is genuinely
    /// unknown, never when it exists but is the wrong kind of thing (a
    /// wrong "add `import ...;`" hint would be worse than none).
    fn resolve_type_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &omega_parser::prelude::Path,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        if let Some(head_type) = self.context.find_defined_type(&path.head).cloned() {
            return self.resolve_type_member(node_id, span, &head_type, &path.tail);
        }

        // A plain (non-generic) *type* import alias resolves outright,
        // exactly the same lazy-alias treatment `Context::resolve_type`
        // gives an unqualified `Type::Named` -- see its own comment for why
        // this can no longer be caught by `find_defined_type` above.
        let alias = self.resolve_alias_or_error(node_id, span, &path.head)?;
        if let Some(ImportTarget::Item(ResolvedItem::Type(t))) = alias {
            return self.resolve_type_member(node_id, span, &t, &path.tail);
        }
        let absolute: Vec<Ident> = match alias {
            Some(ImportTarget::GenericItem(absolute)) | Some(ImportTarget::Module(absolute)) => absolute,
            _ => self.module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect(),
        };
        let kind = match self.resolver.resolve_item(&absolute, &[], true) {
            Ok(ResolvedItem::Type(t)) => {
                return self.resolve_type_member(node_id, span, &t, &path.tail);
            }
            Ok(ResolvedItem::Value { .. }) => AnalysisErrorKind::NotAModule { name: path.head.clone() },
            // The head names nothing at all -- an unimported module, or a
            // typo of a struct/module that does exist; suggest whichever
            // actually does.
            Err(ResolveError::UnknownItem { .. }) => AnalysisErrorKind::UndefinedPathHead {
                name: path.head.clone(),
                similar_module: self.similar_import_alias(&path.head),
                similar_type: self.context.similar_type_name(&path.head).or_else(|| {
                    self.resolver.similar_item_name(&self.module_path, &path.head, ItemNamespace::Type)
                }),
            },
            // The head *does* name something here (a failed item, an
            // uninstantiated generic, ...) -- report that, precisely.
            Err(e) => AnalysisErrorKind::ModuleResolution(e),
        };
        self.errors.push(AnalysisError::new(node_id, span, kind));
        None
    }

    /// A place root whose path carries explicit generic arguments
    /// (`Optional<u32>::Some`, `List<u8>::new`, `sum_generic<f64>`): the
    /// argumented prefix resolves through the same instantiating
    /// `resolve_item` query every other generic reference uses, and
    /// whatever one segment may follow it resolves as a member of the
    /// resulting type (`resolve_type_member`). An instantiated *value* (a
    /// generic function referenced with explicit arguments) is legal only
    /// with nothing after it.
    fn resolve_generic_args_place(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let segments = expr_path.path.segments();
        let rest = &segments[expr_path.args_at + 1..];
        if rest.len() > 1 {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::GenericPathTooDeep { r#type: segments[expr_path.args_at].clone() },
            ));
            return None;
        }

        let type_args = self.resolve_generic_arg_list(node_id, span, expr_path)?;
        let absolute = self.generic_prefix_absolute(node_id, span, &segments[..=expr_path.args_at])?;
        match self.resolver.resolve_item(&absolute, &type_args, true) {
            Ok(ResolvedItem::Type(_)) if rest.is_empty() => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAValue(absolute)));
                None
            }
            Ok(ResolvedItem::Type(t)) => self.resolve_type_member(node_id, span, &t, rest),
            Ok(ResolvedItem::Value { r#type, storage, decl_id }) if rest.is_empty() => {
                let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                Some((root, r#type))
            }
            Ok(ResolvedItem::Value { .. }) => {
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::NotAModule { name: segments[expr_path.args_at].clone() },
                ));
                None
            }
            Err(e) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                None
            }
        }
    }

    /// Resolves an `ExprPath`'s written `<T, ...>` arguments -- always
    /// indirect, same reasoning as `Type::Generic`'s argument resolution in
    /// `Context::resolve_type`.
    fn resolve_generic_arg_list(&mut self, node_id: HirId, span: Span, expr_path: &ExprPath) -> Option<Vec<ResolvedType>> {
        self.analyze_all(&expr_path.generic_args, |this, arg| {
            this.resolve_type_or_error(node_id, span, arg, true)
        })
    }

    /// The absolute item path of an expression path's generic-argumented
    /// *prefix* (`Optional` in `Optional<u32>::Some`, `mymodule::List` in
    /// `mymodule::List<u8>::new`) -- the same alias-vs-own-module priority
    /// `Context::resolve_absolute_item_path` applies to type positions.
    fn generic_prefix_absolute(&mut self, node_id: HirId, span: Span, prefix: &[Ident]) -> Option<Vec<Ident>> {
        if let [single] = prefix {
            if let Some(ImportTarget::GenericItem(absolute)) = self.resolve_alias_or_error(node_id, span, single)? {
                return Some(absolute);
            }
            return Some(self.module_path.iter().cloned().chain(std::iter::once(single.clone())).collect());
        }
        let path = omega_parser::prelude::Path { head: prefix[0].clone(), tail: prefix[1..].to_vec() };
        match self.resolve_alias_or_error(node_id, span, &path.head)? {
            Some(ImportTarget::Module(target)) => {
                Some(target.into_iter().chain(path.tail.iter().cloned()).collect())
            }
            _ => {
                let similar_module = self.similar_import_alias(&path.head);
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::UndefinedPathHead {
                        name: path.head.clone(),
                        similar_module,
                        similar_type: self.context.similar_type_name(&path.head),
                    },
                ));
                None
            }
        }
    }

    /// `Type::member` -- resolves `rest` (the path segments after the type's
    /// own name, always non-empty) against `r#type`'s members. For a struct
    /// that can only be a static function; for an enum it's a variant
    /// (producing a whole constructed value -- the unit form, so only valid
    /// for a body-less variant) or a static function. A function declared
    /// *without* `self` is static: callable through the type's name alone,
    /// with no instance. A static function resolves to an ordinary
    /// `Storage::Function` place root, exactly what a member-call callee
    /// resolves to; a unit variant resolves to a `CheckedPlaceRoot::Expr`
    /// construction -- codegen needs no new machinery for either.
    fn resolve_type_member(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: &ResolvedType,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let member = &rest[0];
        let (type_name, method, missing_member_error) = match r#type {
            ResolvedType::Struct(cell) => {
                let struct_type = cell.borrow();
                let method = struct_type
                    .functions
                    .iter()
                    .find(|(name, _)| name == member)
                    .map(|(_, method)| method.clone());
                let similar = match method {
                    Some(_) => None,
                    None => best_match(member, struct_type.functions.iter().map(|(name, _)| name)),
                };
                let missing = AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: struct_type.name.clone(),
                    function: member.clone(),
                    similar,
                };
                (struct_type.name.clone(), method, missing)
            }
            ResolvedType::Union(cell) => {
                let union_type = cell.borrow();
                let method = union_type
                    .functions
                    .iter()
                    .find(|(name, _)| name == member)
                    .map(|(_, method)| method.clone());
                let similar = match method {
                    Some(_) => None,
                    None => best_match(member, union_type.functions.iter().map(|(name, _)| name)),
                };
                let missing = AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: union_type.name.clone(),
                    function: member.clone(),
                    similar,
                };
                (union_type.name.clone(), method, missing)
            }
            ResolvedType::Enum { cell, .. } => {
                // A variant wins over a same-named function -- analysis of
                // the definition would ideally forbid the collision, but
                // resolution still needs a deterministic order.
                if let Some((variant_index, variant)) = {
                    let found = cell.borrow().variant(member).map(|(i, v)| (i, v.clone()));
                    found
                } {
                    return self.resolve_unit_variant(node_id, span, cell, variant_index, &variant, rest);
                }
                let e = cell.borrow();
                let method = e
                    .functions
                    .iter()
                    .find(|(name, _)| name == member)
                    .map(|(_, method)| method.clone());
                let missing = AnalysisErrorKind::NoSuchEnumMember {
                    r#enum: e.name.clone(),
                    name: member.clone(),
                    similar_variant: best_match(member, e.variants.iter().map(|v| &v.name)),
                    similar_function: best_match(member, e.functions.iter().map(|(name, _)| name)),
                };
                (e.name.clone(), method, missing)
            }
            other => {
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::StaticAccessOnNonStruct { found: other.clone() },
                ));
                return None;
            }
        };

        let Some(method) = method else {
            self.errors.push(AnalysisError::new(node_id, span, missing_member_error));
            return None;
        };
        if rest.len() > 1 {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::StructPathTooDeep { r#struct: type_name, function: member.clone() },
            ));
            return None;
        }
        if method.fn_type.is_member_function {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::MemberFunctionWithoutInstance {
                    r#struct: type_name,
                    function: member.clone(),
                },
            ));
            return None;
        }

        let fn_type = ResolvedType::Function(method.fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id: method.decl_id,
            storage: Storage::Function,
            r#type: fn_type.clone(),
        };
        Some((root, fn_type))
    }

    /// `Enum::Variant` in value position -- the unit construction. Only a
    /// variant with no fields at all -- neither its own body fields nor
    /// (now) the enum's shared dynamic fields -- has one (there is no
    /// implicit zeroing to fill a body with); the result is an ordinary
    /// expression place root whose type statically knows its variant.
    fn resolve_unit_variant(
        &mut self,
        node_id: HirId,
        span: Span,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        variant_index: usize,
        variant: &ResolvedEnumVariant,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        if rest.len() > 1 {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::GenericPathTooDeep { r#type: variant.name.clone() },
            ));
            return None;
        }
        let dynamic_field_names: Vec<Ident> = cell.borrow().dynamic_fields.iter().map(|(n, _)| n.clone()).collect();
        if !dynamic_field_names.is_empty() || !variant.fields.is_empty() {
            let fields =
                dynamic_field_names.into_iter().chain(variant.fields.iter().map(|(name, _)| name.clone())).collect();
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::EnumVariantMissingBody {
                    r#enum: cell.borrow().name.clone(),
                    variant: variant.name.clone(),
                    fields,
                },
            ));
            return None;
        }
        let r#type = ResolvedType::Enum { cell: cell.clone(), variant: Some(variant_index) };
        let construct = CheckedExprNode {
            id: node_id,
            span,
            r#type: r#type.clone(),
            kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct { variant_index, fields: vec![] }),
        };
        Some((CheckedPlaceRoot::Expr(Box::new(construct)), r#type))
    }

    /// Resolves a place's root, then folds over its projections in source
    /// order, resolving field/index/deref projections against the running
    /// type and recording the exact resolved shape (field index, item/
    /// pointee type) so codegen never has to re-search or re-derive them.
    ///
    /// Also computes whether the *whole place* may be written to, in the
    /// same walk: it starts as the root's own mutability (a local/global
    /// binding's `VarBinding::mutable`; always `false` for anything reached
    /// through cross-module/qualified resolution, conservatively -- nothing
    /// in this language yet threads a real flag through `ResolvedItem`, and
    /// `false` is the safe default for "immutable unless proven otherwise"),
    /// and is *overwritten* (never combined) every time a `Deref` --
    /// explicit or the seamless one `resolve_field_projection` inserts for
    /// `ptr.field` -- or a `Slice` index is processed, by that pointer's/
    /// slice's own `mutable` flag: going through a pointer resets the
    /// mutability basis to that specific pointer's, regardless of what came
    /// before. A field access or an index into inline storage (`Array`/
    /// `SizedArray`, which aren't fat pointers) never changes it -- it
    /// simply inherits whatever the base's mutability already was.
    fn analyze_place(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlace, ResolvedType, bool)> {
        let (root, mut current_type, mut mutable) = match &place.root {
            // A path with explicit generic arguments (`Optional<u32>::Some`,
            // `sum_generic<f64>`) -- resolved through the instantiating
            // machinery; see `resolve_generic_args_place`.
            HirPlaceRoot::Path(expr_path) if expr_path.plain().is_none() => {
                let (root, r#type) = self.resolve_generic_args_place(node_id, span, expr_path)?;
                (root, r#type, false)
            }
            // An unqualified place root -- a local (function-body-level)
            // binding wins if there is one; otherwise this is a same-module
            // top-level reference, resolved the exact same way a qualified
            // cross-module one is, with `module_path` supplying the
            // implicit prefix (see `resolve_qualified_value`). Values never
            // need the indirect/in-progress distinction type resolution
            // does -- only a named *type* can ever be legitimately
            // mid-collection when referenced (see
            // `ModuleResolver::resolve_item`'s doc comment) -- so this
            // always passes `true`.
            HirPlaceRoot::Path(expr_path) if expr_path.path.is_unqualified() => {
                let path = &expr_path.path;
                let ident = &path.head;
                if let Some(binding) = self.context.find_variable(ident) {
                    let root = CheckedPlaceRoot::Variable {
                        decl_id: binding.decl_id,
                        storage: binding.storage,
                        r#type: binding.r#type.clone(),
                    };
                    (root, binding.r#type.clone(), binding.mutable)
                } else {
                    // An import alias, lazily resolved (see `resolve_alias`)
                    // -- a plain item *value* alias resolves outright
                    // (`bind_imported_item` used to pre-materialize this
                    // into `find_variable` above; now it's resolved on the
                    // spot); a *generic* item alias takes priority over the
                    // implicit own-module prefix, exactly like type position
                    // does -- this only ever reaches here for a *non-call*
                    // reference to a generic function (a call goes through
                    // `resolve_generic_call` first), which has no way to
                    // supply type arguments; `ensure_item` reports that
                    // uniformly as `GenericArgCountMismatch` rather than
                    // this falling through to (and possibly silently
                    // matching) an unrelated same-named item in this module.
                    // A *type* alias or a bare module alias referenced this
                    // way, like no alias at all, falls through to the
                    // implicit own-module assumption -- `resolve_qualified_value`
                    // reports whichever precise error fits.
                    let alias = self.resolve_alias_or_error(node_id, span, ident)?;
                    if let Some(ImportTarget::Item(ResolvedItem::Value { r#type, storage, decl_id })) = alias {
                        let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                        (root, r#type, false)
                    } else {
                        let (absolute, unqualified) = match alias {
                            Some(ImportTarget::GenericItem(absolute)) => (absolute, None),
                            _ => {
                                let absolute =
                                    self.module_path.iter().cloned().chain(std::iter::once(ident.clone())).collect();
                                (absolute, Some(ident))
                            }
                        };
                        let (root, r#type) =
                            self.resolve_qualified_value(node_id, span, absolute, unqualified, expected)?;
                        (root, r#type, false)
                    }
                }
            }
            // A qualified place root -- either module-qualified
            // (`mymodule::thing::foo`, head an imported module alias) or
            // type-qualified (`MyStruct::do_thing`/`MyEnum::Variant`, head a
            // type, the tail one of its members). A module alias wins when
            // both could apply, preserving the module interpretation
            // unchanged; a head that's neither is reported by
            // `resolve_type_qualified_value` with the most precise error it
            // can determine.
            HirPlaceRoot::Path(expr_path) => {
                let path = &expr_path.path;
                let alias = self.resolve_alias_or_error(node_id, span, &path.head)?;
                let (root, r#type) = match alias {
                    Some(ImportTarget::Module(target)) => {
                        let absolute: Vec<Ident> = target.into_iter().chain(path.tail.iter().cloned()).collect();
                        self.resolve_qualified_value(node_id, span, absolute, None, expected)?
                    }
                    _ => self.resolve_type_qualified_value(node_id, span, path)?,
                };
                (root, r#type, false)
            }
            HirPlaceRoot::Expr(expr) => {
                let checked_expr = self.analyze_expr(expr, None)?;
                let r#type = checked_expr.r#type.clone();
                (CheckedPlaceRoot::Expr(Box::new(checked_expr)), r#type, false)
            }
        };

        let mut projections = Vec::with_capacity(place.projections.len());
        for projection in &place.projections {
            match projection {
                HirProjection::FieldAccess(field) => {
                    current_type = self.resolve_field_projection(
                        node_id,
                        span,
                        &mut projections,
                        &current_type,
                        field,
                        &mut mutable,
                    )?;
                }
                HirProjection::Index(index_expr) => {
                    let checked_index = self.analyze_expr(index_expr, None)?;
                    // `Array` (the legacy thin-pointer unsized form, e.g.
                    // `argv`) and `SizedArray` are indexable inline storage
                    // (mutability unchanged); `Slice` is a fat pointer whose
                    // own `mutable` flag resets it, exactly like `Deref`
                    // below -- codegen tells the three apart itself (see
                    // `resolve_place_storage`'s `Index` arm) using the exact
                    // same `current_type` this match is on.
                    let item_type = match current_type {
                        ResolvedType::Array(item_type) => *item_type,
                        ResolvedType::SizedArray(item_type, _) => *item_type,
                        ResolvedType::Slice { item, mutable: slice_mutable } => {
                            mutable = slice_mutable;
                            *item
                        }
                        found => {
                            self.errors
                                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAnArray { found }));
                            return None;
                        }
                    };
                    projections.push(CheckedProjection::Index {
                        index_expr: Box::new(checked_index),
                        item_type: item_type.clone(),
                    });
                    current_type = item_type;
                }
                HirProjection::Deref => {
                    let ResolvedType::Pointer { pointee, mutable: pointer_mutable } = current_type else {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            span,
                            AnalysisErrorKind::NotAPointer { found: current_type },
                        ));
                        return None;
                    };
                    mutable = pointer_mutable;
                    let inner_type = *pointee;
                    projections.push(CheckedProjection::Deref { r#type: inner_type.clone() });
                    current_type = inner_type;
                }
            }
        }

        Some((CheckedPlace { root, projections }, current_type, mutable))
    }

    fn resolve_callee(&mut self, callee: &HirExprNode, args: &[HirExprNode]) -> Option<CalleeResolution> {
        if let HirExpr::Place(place) = &callee.expr
            && let Some(HirProjection::FieldAccess(field)) = place.projections.last()
        {
            let base_place = HirPlace {
                root: place.root.clone(),
                projections: place.projections[..place.projections.len() - 1].to_vec(),
            };
            let (checked_base, base_type, base_mutable) = self.analyze_place(callee.id, callee.span, &base_place, None)?;

            // Dynamic dispatch: `base`'s type is a `spec *Spec` value, not a
            // concrete struct/enum/union -- `method` is looked up in the
            // spec's own flattened function list instead of any one
            // concrete type's `functions` (there is none; the concrete
            // type is erased). See `finish_dynamic_dispatch_call`.
            if let ResolvedType::SpecObject { spec, type_args, .. } = &base_type {
                let spec = spec.clone();
                let type_args = type_args.clone();
                return Some(CalleeResolution::Dynamic(self.finish_dynamic_dispatch_call(
                    callee.id, callee.span, checked_base, &spec, &type_args, field, args,
                )));
            }

            let methods = self.find_methods(&base_type, field);
            if !methods.is_empty() {
                // A function declared without `self` is static -- reached
                // through the struct's name (`MyStruct::f()`), never through
                // an instance; prepending an implicit `self` argument it has
                // no parameter for could only produce nonsense downstream.
                // Only the *member* candidates are even callable this way,
                // so overload resolution (when there's more than one) never
                // needs to consider the static ones at all.
                let member_methods: Vec<ResolvedMethod> =
                    methods.into_iter().filter(|m| m.fn_type.is_member_function).collect();

                let (method, checked_args) = if member_methods.len() > 1 {
                    // Scored without each candidate's own synthesized `self`
                    // param -- `args` is the call's user-written arguments
                    // only, `self` is never itself overload-distinguishing
                    // (every member candidate has exactly one, always
                    // viable). The winning candidate's *real* (with-`self`)
                    // signature is read back from `member_methods` by index
                    // right after.
                    let candidates: Vec<(HirId, ResolvedFunctionType)> = member_methods
                        .iter()
                        .map(|m| {
                            let mut sans_self = m.fn_type.clone();
                            sans_self.params = sans_self.params[1..].to_vec();
                            (m.decl_id, sans_self)
                        })
                        .collect();
                    let (winner, checked) = self.resolve_overload(callee.id, callee.span, field, &candidates, args)?;
                    (member_methods[winner].clone(), Some(checked))
                } else if let Some(only) = member_methods.into_iter().next() {
                    (only, None)
                } else {
                    let dereffed = match &base_type {
                        ResolvedType::Pointer { pointee, .. } => pointee.as_ref(),
                        other => other,
                    };
                    let r#struct = match dereffed {
                        ResolvedType::Struct(cell) => cell.borrow().name.clone(),
                        ResolvedType::Union(cell) => cell.borrow().name.clone(),
                        ResolvedType::Enum { cell, .. } => cell.borrow().name.clone(),
                        _ => unreachable!("find_methods only ever finds methods on struct/union/enum types"),
                    };
                    self.errors.push(AnalysisError::new(
                        callee.id,
                        callee.span,
                        AnalysisErrorKind::StaticFunctionOnInstance { r#struct, function: field.clone() },
                    ));
                    return None;
                };

                // `self`'s own declared type (always the first param, a
                // synthesized `*Self`/`*mut Self` -- see
                // `omega_hir::lower::Lowerer::lower_function_def`) says
                // whether this call needs write access to `base`.
                let self_mutable = match method.fn_type.params.first() {
                    Some((_, ResolvedType::Pointer { mutable, .. })) => *mutable,
                    _ => unreachable!("a member function's first param is always the synthesized self: *Self/*mut Self"),
                };

                // `self` is `&base`/`&mut base` -- or, if `base` is already a
                // pointer, `base` itself, coerced to exactly the pointer
                // shape `self` expects (that's what a seamless deref would
                // have produced anyway, so there's no need to materialize a
                // Deref-then-AddressOf round trip just to get back the same
                // pointer value).
                let self_arg = if let ResolvedType::Pointer { pointee: base_pointee, mutable: base_ptr_mutable } =
                    &base_type
                {
                    if self_mutable && !base_ptr_mutable {
                        self.errors.push(AnalysisError::new(callee.id, callee.span, AnalysisErrorKind::NotMutablePointer));
                        return None;
                    }
                    let pointer_type =
                        ResolvedType::Pointer { pointee: Box::new(base_pointee.widened()), mutable: self_mutable };
                    CheckedExprNode {
                        id: callee.id,
                        span: callee.span,
                        r#type: pointer_type,
                        kind: CheckedExpr::Place(checked_base),
                    }
                } else {
                    if self_mutable {
                        self.require_mutable_place(callee.id, callee.span, &base_place.root, &checked_base, base_mutable)?;
                        // De-assumption, exactly like an explicit `&mut`: a
                        // writable alias to `base` now exists for the
                        // duration of this call.
                        if let Some((ident, ..)) = self.narrowable_place(&base_place) {
                            self.context.widen_variable(&ident);
                        }
                    }
                    // Widened for the same reason an explicit `&`/`&mut`
                    // widens: a method's `self` is `*Self`/`*mut Self`, never
                    // `*Self::Variant`.
                    let pointer_type = ResolvedType::Pointer { pointee: Box::new(base_type.widened()), mutable: self_mutable };
                    CheckedExprNode {
                        id: callee.id,
                        span: callee.span,
                        r#type: pointer_type,
                        kind: CheckedExpr::AddressOf(CheckedAddressOf { place: checked_base }),
                    }
                };

                let callee_expr = CheckedExprNode {
                    id: callee.id,
                    span: callee.span,
                    r#type: ResolvedType::Function(method.fn_type.clone()),
                    kind: CheckedExpr::Place(CheckedPlace {
                        root: CheckedPlaceRoot::Variable {
                            decl_id: method.decl_id,
                            storage: Storage::Function,
                            r#type: ResolvedType::Function(method.fn_type.clone()),
                        },
                        projections: vec![],
                    }),
                };

                return Some(CalleeResolution::Ordinary(ResolvedCallee {
                    callee: callee_expr,
                    fn_type: method.fn_type,
                    implicit_self: Some(self_arg),
                    checked_args,
                }));
            }

            // Not a method -- finish resolving the ordinary field access
            // using the base place we already have, instead of re-resolving
            // the whole place from scratch (which would risk reporting the
            // base's errors, e.g. an undefined variable, twice).
            let CheckedPlace { root, mut projections } = checked_base;
            let field_type = self.resolve_field_projection(
                callee.id,
                callee.span,
                &mut projections,
                &base_type,
                field,
                &mut false,
            )?;
            let checked_callee = CheckedExprNode {
                id: callee.id,
                span: callee.span,
                r#type: field_type.clone(),
                kind: CheckedExpr::Place(CheckedPlace { root, projections }),
            };
            let ResolvedType::Function(fn_type) = field_type else {
                self.errors
                    .push(AnalysisError::new(callee.id, callee.span, AnalysisErrorKind::UnresolvedCallee));
                return None;
            };
            return Some(CalleeResolution::Ordinary(ResolvedCallee {
                callee: checked_callee,
                fn_type,
                implicit_self: None,
                checked_args: None,
            }));
        }

        let checked_callee = self.analyze_expr(callee, None)?;
        let ResolvedType::Function(fn_type) = checked_callee.r#type.clone() else {
            self.errors
                .push(AnalysisError::new(callee.id, callee.span, AnalysisErrorKind::UnresolvedCallee));
            return None;
        };
        Some(CalleeResolution::Ordinary(ResolvedCallee {
            callee: checked_callee,
            fn_type,
            implicit_self: None,
            checked_args: None,
        }))
    }

    /// Finishes resolving `base.field(args)` once `resolve_callee` has
    /// already determined `base`'s type is `spec<type_args> *_` --
    /// `field` is looked up by *position* in the spec's flattened
    /// function list (`flatten_spec`), which is exactly the vtable slot
    /// order `Codegen`'s vtable builder uses too, so the two always agree.
    /// `Self` is bound to a placeholder (`Void`) purely to give the
    /// resolved signature the right *leaf shape* for codegen -- a
    /// pointer's own leaf count never depends on what it points to, so
    /// this is sound for every purpose the resolved `fn_type` is used for
    /// here (argument type-checking, `self`'s param count). Argument
    /// checking mirrors `resolve_callee`'s/`FunctionCall`'s own ordinary
    /// loop (including `coerce_to_expected`, so a `spec *Animal` argument
    /// passed through to a *further* dynamic call still coerces/no-ops
    /// correctly) -- kept separate rather than shared, since there's no
    /// ordinary `callee`/`fn_type` pairing this shape can reuse that loop
    /// through.
    fn finish_dynamic_dispatch_call(
        &mut self,
        id: HirId,
        span: Span,
        base: CheckedPlace,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        type_args: &[ResolvedType],
        field: &Ident,
        args: &[HirExprNode],
    ) -> Option<CheckedExprNode> {
        let self_placeholder = ResolvedType::Void;
        let flattened = self.flatten_spec(id, span, spec, type_args, &self_placeholder)?;
        let Some(slot_index) = flattened.iter().position(|f| &f.name == field) else {
            let spec_name = spec.borrow().name.clone();
            self.errors.push(AnalysisError::new(
                id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction { spec: spec_name, function: field.clone() },
            ));
            return None;
        };
        let fn_type = flattened[slot_index].fn_type.clone();
        // params[0] is the synthesized self -- never counted against the
        // user's own written arguments, exactly like an ordinary method
        // call's implicit self.
        let param_types = fn_type.params[1..].to_vec();

        if args.len() != param_types.len() {
            self.errors.push(AnalysisError::new(
                id,
                span,
                AnalysisErrorKind::WrongArgumentCount { expected: param_types.len(), found: args.len() },
            ));
            return None;
        }

        let mut checked_args = Vec::with_capacity(args.len());
        let mut ok = true;
        for (arg, (_, expected_type)) in args.iter().zip(&param_types) {
            let Some(checked_arg) = self.analyze_expr(arg, Some(expected_type)) else {
                ok = false;
                continue;
            };
            let checked_arg = self.coerce_to_expected(Some(expected_type), checked_arg);
            if !expected_type.accepts(&checked_arg.r#type) {
                self.errors.push(AnalysisError::new(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: checked_arg.r#type.clone(),
                    },
                ));
                ok = false;
                continue;
            }
            checked_args.push(checked_arg);
        }
        if !ok {
            return None;
        }

        Some(CheckedExprNode {
            id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::DynamicCall(CheckedDynamicCall { base, slot_index, fn_type, args: checked_args }),
        })
    }

    /// If `call`'s callee is `Type::function(args)` -- a static function
    /// reached through a struct/enum/union's own name, never an instance --
    /// where `function` names 2+ overloaded (non-member) candidates,
    /// resolves the whole call here via the same `resolve_overload`
    /// machinery `resolve_callee`'s method-call branch uses, with the
    /// identical `Option<Option<_>>` "handled or fall through" convention.
    /// Returns plain `None` for anything that isn't this exact shape --
    /// most importantly a name with 0 or 1 static candidates, which falls
    /// through to `resolve_type_member`'s existing, completely unchanged
    /// single-candidate path. Deliberately scoped to a *locally visible*
    /// type name (this module's own, or an imported alias) -- a deeper
    /// module-qualified type path (`module::Type::function`) still resolves
    /// correctly through the ordinary path, just without overload
    /// disambiguation (an intentionally narrow, documented gap: this shape
    /// is rare enough, and resolving its type half needs machinery this
    /// method would otherwise have to duplicate wholesale from
    /// `resolve_qualified_value`).
    fn resolve_overloaded_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Option<Option<CheckedExprNode>> {
        let HirExpr::Place(place) = &call.callee.expr else { return None };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else { return None };
        let path = expr_path.plain()?;
        let [member] = path.tail.as_slice() else { return None };

        // A module alias wins over a type interpretation whenever both
        // could apply -- the same priority `resolve_type_qualified_value`
        // gives it -- so a genuine `module::function` shape (already
        // `resolve_overloaded_call`'s concern) is never misread as
        // `Type::function` here. A silent probe, like the rest of this
        // function -- a real resolution failure here isn't this function's
        // to report; it's left for whichever fallback path ends up actually
        // needing this same alias to surface it.
        let alias = self.resolve_alias(&path.head).ok().flatten();
        if matches!(alias, Some(ImportTarget::Module(_))) {
            return None;
        }

        let r#type = if let Some(t) = self.context.find_defined_type(&path.head) {
            t.clone()
        } else if let Some(ImportTarget::Item(ResolvedItem::Type(t))) = alias {
            t
        } else {
            let absolute: Vec<Ident> =
                self.module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect();
            match self.resolver.resolve_item(&absolute, &[], true) {
                Ok(ResolvedItem::Type(t)) => t,
                _ => return None,
            }
        };

        let all_methods = match &r#type {
            ResolvedType::Struct(cell) => cell.borrow().functions.clone(),
            ResolvedType::Union(cell) => cell.borrow().functions.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().functions.clone(),
            _ => return None,
        };
        let statics: Vec<ResolvedMethod> = all_methods
            .into_iter()
            .filter(|(name, m)| name == member && !m.fn_type.is_member_function)
            .map(|(_, m)| m)
            .collect();
        if statics.len() < 2 {
            return None;
        }

        let candidates: Vec<(HirId, ResolvedFunctionType)> =
            statics.iter().map(|m| (m.decl_id, m.fn_type.clone())).collect();
        let Some((winner, args)) = self.resolve_overload(node_id, span, member, &candidates, &call.args) else {
            return Some(None);
        };
        let (decl_id, fn_type) = candidates[winner].clone();

        let callee = CheckedExprNode {
            id: call.callee.id,
            span: call.callee.span,
            r#type: ResolvedType::Function(fn_type.clone()),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id, storage: Storage::Function, r#type: ResolvedType::Function(fn_type.clone()) },
                projections: vec![],
            }),
        };
        let return_type = *fn_type.return_type.clone();
        Some(Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: return_type,
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee), fn_type, args }),
        }))
    }

    /// If `call`'s callee is a bare (optionally module-qualified) reference
    /// to an *overloaded* name (2+ non-generic top-level functions sharing
    /// it -- see `ModuleResolver::function_overload_signatures`), resolves
    /// the whole call here via argument-driven overload resolution
    /// (`resolve_overload`) instead of the ordinary `resolve_callee`-then-
    /// args pipeline, with the identical `Option<Option<_>>` "handled or
    /// fall through" convention `resolve_generic_call` uses (checked
    /// immediately before it, at this call's own use site). Returns plain
    /// `None` for anything that isn't this exact shape -- most importantly,
    /// a name with 0 or 1 candidates, which is the overwhelming majority of
    /// calls and stays on the completely unchanged ordinary path.
    fn resolve_overloaded_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Option<Option<CheckedExprNode>> {
        let HirExpr::Place(place) = &call.callee.expr else { return None };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else { return None };
        let path = expr_path.plain()?;

        if path.is_unqualified() && self.context.find_variable(&path.head).is_some() {
            return None;
        }

        let absolute: Vec<Ident> = if path.is_unqualified() {
            self.module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect()
        } else {
            match self.resolve_alias(&path.head).ok().flatten() {
                Some(ImportTarget::Module(target)) => target.into_iter().chain(path.tail.iter().cloned()).collect(),
                _ => return None,
            }
        };
        let (name, module_path) = absolute.split_last()?;

        let candidates = match self.resolver.function_overload_signatures(module_path, name) {
            Ok(Some(candidates)) => candidates,
            Ok(None) => return None,
            Err(e) => {
                self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                return Some(None);
            }
        };

        let Some((winner, args)) = self.resolve_overload(node_id, span, name, &candidates, &call.args) else {
            return Some(None);
        };
        let (decl_id, fn_type) = candidates[winner].clone();

        let callee = CheckedExprNode {
            id: call.callee.id,
            span: call.callee.span,
            r#type: ResolvedType::Function(fn_type.clone()),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id, storage: Storage::Function, r#type: ResolvedType::Function(fn_type.clone()) },
                projections: vec![],
            }),
        };
        let return_type = *fn_type.return_type.clone();
        Some(Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: return_type,
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee), fn_type, args }),
        }))
    }

    /// Resolves a call against 2+ same-named candidates by argument type --
    /// shared by `resolve_overloaded_call` (top-level functions) and, once
    /// wired in, struct/enum/union method calls (`resolve_callee`'s
    /// method-call branch, `resolve_type_member`'s static-function branch).
    /// `candidates` pairs each overload's own identity (a `HirId` --
    /// whatever the caller needs to build the resolved callee/method
    /// reference) with its resolved signature; `args` are the call's own
    /// raw (not yet analyzed) argument expressions.
    ///
    /// Every argument that isn't an `adaptable_literal` (see its own doc
    /// comment) is analyzed exactly once, up front -- its resolved type
    /// can't depend on which candidate wins, so this is what avoids
    /// double-analyzing (and double-erroring on) a fixed-type argument
    /// across every candidate's viability check. An adaptable-literal
    /// argument is instead scored per candidate via `literal_overload_fit`,
    /// silently (no errors for a candidate that turns out not to win): a
    /// candidate is viable iff every argument fits its corresponding
    /// parameter, and its *score* is how many adaptable-literal arguments
    /// needed a type other than their own natural default (`i32`/`f64`) to
    /// fit -- 0 for "every literal stayed at its default." The unique
    /// minimum-score viable candidate wins; zero viable candidates is
    /// `NoMatchingOverload`, two or more tied at the minimum is
    /// `AmbiguousOverload`. Once a winner is picked, its own
    /// adaptable-literal arguments are analyzed for real (the only point
    /// they're actually committed to a concrete type).
    fn resolve_overload(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[(HirId, ResolvedFunctionType)],
        args: &[HirExprNode],
    ) -> Option<(usize, Vec<CheckedExprNode>)> {
        let mut fixed: Vec<Option<CheckedExprNode>> = Vec::with_capacity(args.len());
        for arg in args {
            fixed.push(if Self::adaptable_literal(arg) { None } else { Some(self.analyze_expr(arg, None)?) });
        }

        let mut viable: Vec<(usize, u32)> = Vec::new();
        for (i, (_, fn_type)) in candidates.iter().enumerate() {
            if fn_type.is_variadic || fn_type.params.len() != args.len() {
                continue;
            }
            let mut score = 0u32;
            let mut ok = true;
            for ((_, param_type), (arg, fixed_arg)) in fn_type.params.iter().zip(args.iter().zip(&fixed)) {
                match fixed_arg {
                    Some(checked) => {
                        if !param_type.accepts(&checked.r#type) {
                            ok = false;
                            break;
                        }
                    }
                    None => match Self::literal_overload_fit(arg, param_type) {
                        Some(true) => {}
                        Some(false) => score += 1,
                        None => {
                            ok = false;
                            break;
                        }
                    },
                }
            }
            if ok {
                viable.push((i, score));
            }
        }

        let Some(min_score) = viable.iter().map(|&(_, s)| s).min() else {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::NoMatchingOverload {
                    name: name.clone(),
                    candidates: candidates.iter().map(|(_, t)| t.clone()).collect(),
                },
            ));
            return None;
        };
        let winners: Vec<usize> = viable.iter().filter(|&&(_, s)| s == min_score).map(|&(i, _)| i).collect();
        let winner = match winners.as_slice() {
            [only] => *only,
            _ => {
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::AmbiguousOverload {
                        name: name.clone(),
                        candidates: winners.iter().map(|&i| candidates[i].1.clone()).collect(),
                    },
                ));
                return None;
            }
        };

        let winner_params = &candidates[winner].1.params;
        let mut final_args = Vec::with_capacity(args.len());
        for (arg, fixed_arg) in args.iter().zip(fixed) {
            let checked = match fixed_arg {
                Some(checked) => checked,
                None => {
                    let index = final_args.len();
                    self.analyze_expr(arg, Some(&winner_params[index].1))?
                }
            };
            final_args.push(checked);
        }

        Some((winner, final_args))
    }

    /// Whether an `adaptable_literal` argument fits `target` for overload-
    /// viability purposes, and -- if so -- whether `target` is exactly the
    /// literal's own natural default type (`i32`/`f64`); see
    /// `resolve_overload`'s doc comment for how the result is used.
    /// `None` if it doesn't fit at all (wrong numeric kind/family, or out
    /// of range for `target`'s width). Deliberately silent -- never pushes
    /// an error, since a candidate this rejects might not be the one the
    /// call ultimately resolves to.
    fn literal_overload_fit(arg: &HirExprNode, target: &ResolvedType) -> Option<bool> {
        let n = match &arg.expr {
            HirExpr::Number(n) => n,
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => n,
                _ => return None,
            },
            _ => return None,
        };
        let target_kind = target.numeric_kind()?;
        if matches!(target_kind, NumericKind::Float(_)) != n.fractional_part.is_some() {
            return None;
        }
        parse_number_literal(n, target_kind).ok()?;
        let default = if n.fractional_part.is_some() { ResolvedType::F64 } else { ResolvedType::I32 };
        Some(*target == default)
    }

    /// If `call`'s callee is a bare (optionally module-qualified) reference
    /// to a *generic* function, resolves the whole call here via duck-typed,
    /// argument-driven type inference instead of the ordinary
    /// `resolve_callee`-then-args pipeline, and returns `Some(result)`
    /// (`result` itself `None` on a reported error, `Some` on success) --
    /// the caller must not fall through to the ordinary path either way, to
    /// avoid re-analyzing/double-reporting. Returns plain `None` (untouched)
    /// for anything that isn't this shape, so the caller proceeds with the
    /// ordinary path exactly as if this were never called:
    ///
    /// - a method-call shape (`base.method(...)`, i.e. the callee's last
    ///   projection is a `FieldAccess`) -- struct generics are always
    ///   explicit (`List<u32>`), so by the time a value of that type exists,
    ///   its methods are already fully monomorphized; no special call-site
    ///   handling is needed there at all;
    /// - a callee that isn't a bare/qualified path with zero projections;
    /// - a path shadowed by a local (function-body-level) binding -- always
    ///   wins, and is never generic (only top-level items can be);
    /// - a qualified path whose head isn't a recognized import alias -- left
    ///   for the ordinary path to report `UndefinedVariable`;
    /// - `generic_function_signature` reporting this isn't a generic
    ///   function (including "doesn't exist," or a generic *struct* --
    ///   neither is this call's concern).
    fn resolve_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Option<Option<CheckedExprNode>> {
        let HirExpr::Place(place) = &call.callee.expr else { return None };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else { return None };
        // Explicit generic arguments already pin the instantiation -- the
        // ordinary path resolves them (see `resolve_generic_args_place`),
        // with no argument-driven deduction wanted or needed.
        let Some(path) = expr_path.plain() else { return None };

        if path.is_unqualified() && self.context.find_variable(&path.head).is_some() {
            return None;
        }

        let absolute: Vec<Ident> = if path.is_unqualified() {
            match self.resolve_alias(&path.head).ok().flatten() {
                Some(ImportTarget::GenericItem(absolute)) => absolute,
                _ => self.module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect(),
            }
        } else {
            match self.resolve_alias(&path.head).ok().flatten() {
                Some(ImportTarget::Module(target)) => target.into_iter().chain(path.tail.iter().cloned()).collect(),
                _ => return None,
            }
        };

        let sig: GenericSignature = match self.resolver.generic_function_signature(&absolute) {
            Ok(Some(sig)) => sig,
            Ok(None) => return None,
            Err(_) => return None,
        };

        Some(self.finish_generic_call(node_id, span, call, &absolute, &sig))
    }

    /// The actual work behind `resolve_generic_call`, once it's confirmed
    /// `call`'s callee genuinely names a generic function at `absolute` --
    /// split out so `resolve_generic_call` can stay a single `?`-chained
    /// "does this even apply" check.
    fn finish_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        absolute: &[Ident],
        sig: &GenericSignature,
    ) -> Option<CheckedExprNode> {
        let mut checked_args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            checked_args.push(self.analyze_expr(arg, None)?);
        }

        let mut subst = HashMap::new();
        for (raw_type, arg) in sig.params.iter().zip(&checked_args) {
            unify_generic_type(&sig.generics, raw_type, &arg.r#type, &mut subst);
        }

        let mut type_args = Vec::with_capacity(sig.generics.len());
        for generic in &sig.generics {
            match subst.get(generic) {
                // Widened: a deduced `T` must never carry a caller-specific
                // enum-variant refinement -- `T = MyEnum`, not
                // `T = MyEnum::Second` (which would mint a spurious extra
                // instantiation per variant).
                Some(resolved) => type_args.push(resolved.widened()),
                None => {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedGenericParam(generic.clone()),
                    ));
                    return None;
                }
            }
        }

        let (fn_type, storage, decl_id) = match self.resolver.resolve_item(absolute, &type_args, true) {
            Ok(ResolvedItem::Value { r#type: ResolvedType::Function(fn_type), storage, decl_id }) => {
                (fn_type, storage, decl_id)
            }
            Ok(_) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::UnresolvedCallee));
                return None;
            }
            Err(e) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                return None;
            }
        };

        if checked_args.len() != fn_type.params.len() && !fn_type.is_variadic {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount { expected: fn_type.params.len(), found: checked_args.len() },
            ));
            return None;
        }
        for (arg, (_, expected_type)) in checked_args.iter().zip(&fn_type.params) {
            if !expected_type.accepts(&arg.r#type) {
                self.errors.push(AnalysisError::new(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: arg.r#type.clone(),
                    },
                ));
                return None;
            }
        }

        let callee_node = CheckedExprNode {
            id: call.callee.id,
            span: call.callee.span,
            r#type: ResolvedType::Function(fn_type.clone()),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id, storage, r#type: ResolvedType::Function(fn_type.clone()) },
                projections: vec![],
            }),
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee_node), fn_type, args: checked_args }),
        })
    }

    /// Picks the concrete type an *unsuffixed* literal resolves to: `expected`
    /// (untyped-constant inference -- see `Self::adaptable_literal`'s doc
    /// comment) if it's given and its numeric family agrees with the
    /// literal's own -- `Float` iff the literal was written with a
    /// fractional part, never the other way around (an int-kind literal
    /// never silently becomes a float, only a same-family width/signedness
    /// adapts) -- else today's plain i32/f64 default (mirroring Rust's own
    /// literal defaults). An explicit suffix always wins outright and never
    /// reaches this at all (see `analyze_number`).
    fn default_or_expected_number_type(n: &NumberExpr, expected: Option<&ResolvedType>) -> ResolvedType {
        let default = if n.fractional_part.is_some() { ResolvedType::F64 } else { ResolvedType::I32 };
        let Some(expected) = expected else { return default };
        let Some(kind) = expected.numeric_kind() else { return default };
        if matches!(kind, NumericKind::Float(_)) == n.fractional_part.is_some() {
            expected.clone()
        } else {
            default
        }
    }

    /// Resolves a number literal's target type (see
    /// `default_or_expected_number_type`) and parses/range-checks its text
    /// against that type (see `parse_number_literal`). `NumberExpr` keeps
    /// its digits as plain text (see its doc comment) precisely so this is
    /// the *only* place that ever has to interpret them -- codegen just
    /// emits whatever `NumberValue` this produces.
    fn analyze_number(
        &mut self,
        node_id: HirId,
        span: Span,
        n: &NumberExpr,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let invalid_suffix = |this: &mut Self, ident: &Ident| {
            this.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::InvalidNumberType(ident.clone())));
        };

        let resolved_type = match &n.explicit_type {
            Some(explicit_type) => match self.context.resolve_type(
                Type::Named(explicit_type.clone().into()),
                &mut *self.resolver,
                &self.module_path,
                true,
            ) {
                Ok(r#type) if r#type.numeric_kind().is_some() => r#type,
                _ => {
                    invalid_suffix(self, explicit_type);
                    return None;
                }
            },
            None => Self::default_or_expected_number_type(n, expected),
        };
        let kind = resolved_type
            .numeric_kind()
            .expect("just resolved above, or a hardcoded numeric default");

        // A literal written with a decimal point must resolve to a float
        // type; a based (hex/octal/binary) literal never carries one (the
        // grammar has no notation for it), so a float suffix on one (e.g.
        // `0xFFf32`) is rejected here too rather than silently misparsed.
        let is_float = matches!(kind, NumericKind::Float(_));
        if n.fractional_part.is_some() && !is_float {
            let Some(explicit_type) = &n.explicit_type else {
                unreachable!("the default type for a fractional literal is always F64");
            };
            invalid_suffix(self, explicit_type);
            return None;
        }
        if is_float && n.base != NumberBase::Decimal {
            let Some(explicit_type) = &n.explicit_type else {
                unreachable!("the default type is only Float when a fraction was written, which implies Decimal");
            };
            invalid_suffix(self, explicit_type);
            return None;
        }

        let Ok(value) = parse_number_literal(n, kind) else {
            let literal_text = match &n.fractional_part {
                Some(frac) => format!("{}.{}", n.integer_part, frac),
                None => n.integer_part.clone(),
            };
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::NumberLiteralOutOfRange { literal: literal_text, r#type: resolved_type },
            ));
            return None;
        };

        Some(CheckedExprNode { id: node_id, span, r#type: resolved_type, kind: CheckedExpr::Number(value) })
    }

    /// Whether `expr` is a bare (or singly-negated) *unsuffixed* number
    /// literal -- the one expression shape whose concrete type isn't
    /// already pinned by anything written down, so it's the one shape
    /// worth peeking at *before* fully analyzing it: overload resolution's
    /// viability scoring needs to know "is this argument still open to
    /// adapt" without the side effects (errors bound to a specific resolved
    /// type) a real `analyze_expr` call would commit to. `Negate` is peeked
    /// through because it's transparent to a literal's own type (`-100` is
    /// exactly as adaptable as `100`) -- see `HirExpr::Negate`'s arm in
    /// `analyze_expr`, which threads `expected` straight through for the
    /// identical reason.
    fn adaptable_literal(expr: &HirExprNode) -> bool {
        match &expr.expr {
            HirExpr::Number(n) => n.explicit_type.is_none(),
            HirExpr::Negate(inner) => matches!(&inner.expr, HirExpr::Number(n) if n.explicit_type.is_none()),
            _ => false,
        }
    }

    /// A block's own effective type: its tail expression's type, or -- if it
    /// has none -- `Void`, *unless* its last statement unconditionally
    /// diverges (see `stmt_diverges`), in which case the block itself never
    /// actually produces `Void` at its own position (control leaves the
    /// function entirely) -- so it's exempt from whatever type is expected
    /// there, the same way Rust's `!` (never) type unifies with anything.
    /// `None` here means exactly that: "diverges, no constraint," not "has
    /// no type."
    fn block_type(block: &CheckedBlock) -> Option<ResolvedType> {
        match &block.tail {
            Some(tail) if Self::expr_diverges(tail) => None,
            Some(tail) => Some(tail.r#type.clone()),
            None => match block.stmts.last() {
                Some(stmt) if Self::stmt_diverges(stmt) => None,
                _ => Some(ResolvedType::Void),
            },
        }
    }

    /// Whether an expression unconditionally diverges: only an `if`/`else
    /// if`/`else` can (with a genuine `else`, not an implicit empty one)
    /// where *every* branch diverges -- everything else either can't
    /// diverge at all, or (a bare `return`) isn't an expression to begin
    /// with. Needed because such an `if` still gets a concrete (if
    /// degenerate, `Void`) `r#type` of its own during analysis -- there's no
    /// real "never" `ResolvedType` to give it instead -- so whether *it*
    /// diverges has to be re-derived structurally here rather than read off
    /// its `r#type`, the same way `stmt_diverges` re-derives it for a bare
    /// `return` statement.
    fn expr_diverges(expr: &CheckedExprNode) -> bool {
        match &expr.kind {
            CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                let Some(else_branch) = else_branch else { return false };
                branches.iter().all(|(_, b)| Self::block_type(b).is_none())
                    && Self::block_type(else_branch).is_none()
            }
            _ => false,
        }
    }

    /// Whether a statement unconditionally diverges (its block never
    /// actually reaches whatever position it's in): a plain `return`/
    /// `break`/`continue`, or an expression-statement that diverges (see
    /// `expr_diverges` -- currently only a fully-diverging `if`). This is
    /// still a purely syntactic check, not real reachability analysis (e.g.
    /// a `while true { return 1; }` with no way out isn't recognized as
    /// diverging), but "dispatch on a condition and return/break/continue
    /// from every arm" is common enough to be worth recognizing specifically
    /// (see `Codegen::emit_if`'s matching `BlockOutcome::Diverged`
    /// propagation, which this must stay in sync with -- codegen already
    /// builds sound IR for exactly this case).
    fn stmt_diverges(stmt: &CheckedStmt) -> bool {
        match stmt {
            CheckedStmt::Return(_) | CheckedStmt::Break(_) | CheckedStmt::Continue(_) => true,
            CheckedStmt::Expression(expr) => Self::expr_diverges(expr),
            // `defer` never diverges at its own position -- it just marks
            // "reached" and moves on; the deferred body only ever runs later,
            // in the function's epilogue.
            CheckedStmt::Defer(_) => false,
            _ => false,
        }
    }

    /// Every `CheckedStmt` variant's id/span, for anchoring an
    /// `AnalysisWarningKind::UnreachableCode` at whichever statement turns
    /// out to be first made unreachable by a diverging predecessor (see
    /// `truncate_unreachable`).
    fn checked_stmt_id_span(stmt: &CheckedStmt) -> (HirId, Span) {
        match stmt {
            CheckedStmt::Declaration(d) => (d.id, d.span),
            CheckedStmt::ExternDeclaration(d) => (d.id, d.span),
            CheckedStmt::Expression(e) => (e.id, e.span),
            CheckedStmt::Return(e) => (e.id, e.span),
            CheckedStmt::While(w) => (w.id, w.span),
            CheckedStmt::For(f) => (f.id, f.span),
            CheckedStmt::Break(b) => (b.id, b.span),
            CheckedStmt::Continue(c) => (c.id, c.span),
            CheckedStmt::Defer(d) => (d.id, d.span),
        }
    }

    /// Drops every statement after the first one that unconditionally
    /// diverges (see `stmt_diverges`) -- they can never run, and keeping them
    /// in the `CheckedBlock` would make codegen try to emit instructions
    /// into an already-terminated cranelift block (a compiler panic, not a
    /// user-facing error; see `Codegen::emit_block`). Recorded as an
    /// `AnalysisWarningKind::UnreachableCode` rather than an `AnalysisError`:
    /// unlike everything else this pass rejects, dead code doesn't make the
    /// program incorrect, just wasteful -- the same reason real compilers
    /// warn about it instead of refusing to build.
    fn truncate_unreachable(&mut self, mut stmts: Vec<CheckedStmt>) -> Vec<CheckedStmt> {
        let Some(cutoff) = stmts.iter().position(Self::stmt_diverges) else {
            return stmts;
        };
        if let Some(first_dead) = stmts.get(cutoff + 1) {
            let (id, span) = Self::checked_stmt_id_span(first_dead);
            self.warnings.push(AnalysisWarning::new(id, span, AnalysisWarningKind::UnreachableCode));
        }
        stmts.truncate(cutoff + 1);
        stmts
    }

    /// Analyzes a `{ stmts... tail }` block in its own nested scope --
    /// shared by bare codeblock expressions, `if`/`while`/`for` bodies, and
    /// function bodies. Scope is always entered/left even on failure (before
    /// the `?` that can early-return), so an error partway through a block
    /// never leaves the scope stack unbalanced.
    /// `expected` is threaded *only* into the block's own tail expression
    /// (see `analyze_expr`'s doc comment) -- ordinary statements never have
    /// an outer expected type of their own.
    fn analyze_block(&mut self, block: &HirBlock, expected: Option<&ResolvedType>) -> Option<CheckedBlock> {
        self.context.enter_scope();
        let checked_stmts = self.analyze_stmts(&block.stmts);
        let checked_tail = block.tail.as_ref().map(|e| self.analyze_expr(e, expected));
        self.context.leave_scope();

        let stmts = checked_stmts?;
        let tail = match checked_tail {
            Some(t) => Some(Box::new(t?)),
            None => None,
        };

        // `analyze_stmts` already truncated (and warned about) unreachable
        // statements *within* `stmts`; if what's left still ends in
        // something that diverges, a tail expression after it -- if any --
        // is unreachable too, for the same reason.
        let tail = match &tail {
            Some(t) if stmts.last().is_some_and(Self::stmt_diverges) => {
                self.warnings.push(AnalysisWarning::new(t.id, t.span, AnalysisWarningKind::UnreachableCode));
                None
            }
            _ => tail,
        };

        Some(CheckedBlock { stmts, tail })
    }

    /// `++base`/`--base`: validates `base` is a place (like `AddressOf`) of
    /// a numeric type, then desugars directly into `base = base <op> 1` --
    /// an ordinary `Assignment` wrapping a `BinaryOp` over `base`'s own
    /// place and a `Number` matching its exact resolved type/kind. Building
    /// the literal `1` here, rather than going through the parser's
    /// `NumberExpr`/`HirExpr::Number` path, is what lets this work for any
    /// numeric type (an untyped `1` in source would default to `i32`, which
    /// would then fail `BinaryOp`'s "operands must match exactly" rule for
    /// every other numeric type) -- analysis already knows `base`'s exact
    /// type here, so it can build a same-typed constant directly.
    fn analyze_incr_decr(&mut self, node_id: HirId, span: Span, base: &HirExprNode, op: BinaryOp) -> Option<CheckedExprNode> {
        let HirExpr::Place(place) = &base.expr else {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::IncrementTargetNotAPlace));
            return None;
        };
        let (checked_place, place_type, mutable) = self.analyze_place(base.id, base.span, place, None)?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let Some(kind) = place_type.numeric_kind() else {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::InvalidIncrementOperand { r#type: place_type },
            ));
            return None;
        };

        let one = match kind {
            NumericKind::Signed(_) => NumberValue::Signed(1),
            NumericKind::Unsigned(_) => NumberValue::Unsigned(1),
            NumericKind::Float(_) => NumberValue::Float(1.0),
        };
        let one_node = CheckedExprNode { id: node_id, span, r#type: place_type.clone(), kind: CheckedExpr::Number(one) };
        let place_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let sum = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(place_read), right: Box::new(one_node) }),
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type,
            kind: CheckedExpr::Assignment(CheckedAssignment { target: checked_place, value: Box::new(sum) }),
        })
    }

    /// The type-checking core of `left op right`, shared by `HirExpr::
    /// BinaryOp`'s arm and `analyze_compound_assign`'s desugaring (`target
    /// op= value` -> `target = target op value`) -- both already have their
    /// operands analyzed (a compound assignment's `left` is a synthetic
    /// place-read, never itself re-analyzed here), so this only ever
    /// type-checks and combines two already-`CheckedExprNode`s.
    fn analyze_binary_op(
        &mut self,
        node_id: HirId,
        span: Span,
        op: BinaryOp,
        checked_left: CheckedExprNode,
        checked_right: CheckedExprNode,
    ) -> Option<CheckedExprNode> {
        for operand in [&checked_left, &checked_right] {
            if operand.r#type.numeric_kind().is_none() {
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidBinaryOperand { op, r#type: operand.r#type.clone() },
                ));
                return None;
            }
        }

        // No implicit numeric conversions anywhere else in this
        // language (see e.g. `AssignmentTypeMismatch`) -- arithmetic
        // between two different numeric types is no exception.
        if checked_left.r#type != checked_right.r#type {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::BinaryOperandTypeMismatch {
                    left: checked_left.r#type.clone(),
                    left_span: checked_left.span,
                    right: checked_right.r#type.clone(),
                    right_span: checked_right.span,
                },
            ));
            return None;
        }

        // No native float remainder instruction (see
        // `AnalysisErrorKind::FloatRemainder`'s doc comment) --
        // matching C, which requires `fmod`/`fmodf` instead of `%`.
        if op == BinaryOp::Rem && matches!(checked_left.r#type.numeric_kind(), Some(NumericKind::Float(_))) {
            self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::FloatRemainder));
            return None;
        }

        // No native float bitwise/shift instructions either -- same
        // reasoning as `Rem` just above, just for a whole family of ops
        // instead of one.
        if matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr)
            && matches!(checked_left.r#type.numeric_kind(), Some(NumericKind::Float(_)))
        {
            self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::FloatBitwiseOperand));
            return None;
        }

        // A comparison always produces `bool`, regardless of the
        // (still-numeric, still-matching) operand type; an
        // arithmetic op's result is that same operand type.
        let r#type = if op.is_comparison() { ResolvedType::Bool } else { checked_left.r#type.clone() };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(checked_left), right: Box::new(checked_right) }),
        })
    }

    /// `target op= value` -- desugars directly into `target = target op
    /// value`, the same pattern `analyze_incr_decr` already uses for
    /// `++`/`--` (a `BinaryOp` over a place-read and `value`, wrapped in an
    /// ordinary `Assignment`), generalized to a real right-hand side
    /// instead of a synthesized `1`. `value` is analyzed with `expected =
    /// Some(&target_type)` -- the same treatment a plain assignment's value
    /// already gets (`HirExpr::Assignment`'s arm) -- so `a *= 5` adapts an
    /// unsuffixed literal `5` to `a`'s own type rather than defaulting to
    /// `i32`/`f64` and then failing `analyze_binary_op`'s "operands must
    /// match exactly" check.
    fn analyze_compound_assign(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &HirExprNode,
        op: BinaryOp,
        value: &HirExprNode,
    ) -> Option<CheckedExprNode> {
        let HirExpr::Place(place) = &target.expr else {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::CompoundAssignTargetNotAPlace));
            return None;
        };
        let (checked_place, place_type, mutable) = self.analyze_place(target.id, target.span, place, None)?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let checked_value = self.analyze_expr(value, Some(&place_type))?;
        let place_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let combined = self.analyze_binary_op(node_id, span, op, place_read, checked_value)?;

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type,
            kind: CheckedExpr::Assignment(CheckedAssignment { target: checked_place, value: Box::new(combined) }),
        })
    }

    /// `expected` is the concrete type this expression's *result* is about
    /// to flow into, when the caller has one available (a declaration's
    /// annotated type, an assignment's target, a `return`'s function
    /// signature, a call argument's parameter, a struct/union field's
    /// declared type, ...) -- `None` everywhere else. Only a handful of
    /// arms actually consult it: an unsuffixed number literal adapts to it
    /// (untyped-constant inference -- see `default_or_expected_number_type`),
    /// and `ArrayLiteral`/`If`/`Codeblock`/`Negate` thread it further down
    /// into whichever of their own sub-expressions could themselves be
    /// unsuffixed literals. Every other arm ignores it entirely -- this is
    /// deliberately *not* full bidirectional inference, just enough
    /// top-down context for a literal whose own type isn't pinned by an
    /// explicit suffix to adapt instead of defaulting to i32/f64.
    fn analyze_expr(&mut self, node: &HirExprNode, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
        let node_id = node.id;
        let span = node.span;

        match &node.expr {
            HirExpr::Place(place) => {
                let (checked_place, r#type, _mutable) = self.analyze_place(node_id, span, place, expected)?;
                Some(CheckedExprNode { id: node_id, span, r#type, kind: CheckedExpr::Place(checked_place) })
            }

            HirExpr::Number(number_expr) => self.analyze_number(node_id, span, number_expr, expected),

            HirExpr::Bool(b) => {
                Some(CheckedExprNode { id: node_id, span, r#type: ResolvedType::Bool, kind: CheckedExpr::Bool(*b) })
            }

            HirExpr::Char(c) => {
                Some(CheckedExprNode { id: node_id, span, r#type: ResolvedType::Char, kind: CheckedExpr::Char(*c) })
            }

            // A string literal's bytes are raw UTF-8 bytes, not decoded
            // characters -- `*u8`, not `*char` (see `ResolvedType::Char`'s
            // doc comment), the same type C's own string literals decay to.
            HirExpr::String(s) => Some(CheckedExprNode {
                id: node_id,
                span,
                // Immutable, like a C string literal -- writing through it
                // would be just as unsound here as there.
                r#type: ResolvedType::Pointer { pointee: Box::new(ResolvedType::U8), mutable: false },
                kind: CheckedExpr::String(s.0.clone()),
            }),

            // `b"..."` -- a raw byte run with a compile-time-known length,
            // not a null-terminated C string: `*[u8]` (`ResolvedType::
            // Slice`, see `Context::resolve_type`'s `*[T]` special case),
            // never `*u8`. Immutable for the same reason a plain string
            // literal is.
            HirExpr::ByteString(s) => Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: ResolvedType::Slice { item: Box::new(ResolvedType::U8), mutable: false },
                kind: CheckedExpr::ByteString(s.0.clone()),
            }),

            HirExpr::Codeblock(block) => {
                let checked_block = self.analyze_block(block, expected)?;
                let r#type = Self::block_type(&checked_block).unwrap_or(ResolvedType::Void);
                Some(CheckedExprNode { id: node_id, span, r#type, kind: CheckedExpr::Codeblock(checked_block) })
            }

            HirExpr::If(HirIf { branches, else_branch }) => {
                // No `else` at all forces `Void` regardless of branch
                // content below (the "implicit else" is `{}`, matching
                // Rust's identical rule for a possibly-skipped `if`) --
                // branches get no expected type threaded into them in that
                // case, exactly as if this whole feature didn't exist for
                // them: there's no cross-branch value to unify toward.
                let has_else = else_branch.is_some();

                // Earliest-wins unification: branch 0 is always the
                // *anchor* -- the incoming `expected`, if any, otherwise
                // branch 0's own (widened) type once it's analyzed -- and
                // every other branch/`else` is checked *against* that
                // anchor, never the other way around. Unlike the old
                // "peek every branch, use whichever non-literal one is
                // found first" approach, this never lets a *later* branch's
                // already-fixed type (an explicit suffix, a variable, ...)
                // retroactively decide what an earlier adaptable literal
                // infers to -- a later branch only has to *agree* with the
                // anchor (see the mismatch check below), never supply it.
                let mut checked_conds = Vec::with_capacity(branches.len());
                let mut checked_blocks: Vec<CheckedBlock> = Vec::with_capacity(branches.len());
                let mut anchor: Option<ResolvedType> = None;
                for (i, (cond, block)) in branches.iter().enumerate() {
                    let checked_cond = self.analyze_expr(cond, None)?;
                    if checked_cond.r#type != ResolvedType::Bool {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            checked_cond.span,
                            AnalysisErrorKind::NonBoolCondition { r#type: checked_cond.r#type },
                        ));
                        return None;
                    }
                    checked_conds.push(checked_cond);
                    let block_expected = if !has_else {
                        None
                    } else if i == 0 {
                        expected
                    } else {
                        anchor.as_ref()
                    };
                    let checked_block = self.analyze_block(block, block_expected)?;
                    if has_else && i == 0 {
                        anchor = Some(match expected {
                            Some(t) => t.clone(),
                            None => {
                                Self::block_type(&checked_block).map(|t| t.widened()).unwrap_or(ResolvedType::Void)
                            }
                        });
                    }
                    checked_blocks.push(checked_block);
                }
                let checked_else = match else_branch {
                    Some(b) => Some(self.analyze_block(b, anchor.as_ref())?),
                    None => None,
                };

                let checked_branches: Vec<(CheckedExprNode, CheckedBlock)> =
                    checked_conds.into_iter().zip(checked_blocks).collect();

                // What the whole `if` resolves to: the first concrete
                // (non-diverging) type among the branches and the `else`,
                // if any -- diverging branches (ending in `return`) are
                // wildcards, exempt from the check below.
                let branch_kinds: Vec<Option<ResolvedType>> =
                    checked_branches.iter().map(|(_, b)| Self::block_type(b)).collect();
                let else_kind: Option<Option<ResolvedType>> = checked_else.as_ref().map(Self::block_type);

                // Widened: branches producing *different variants* of one
                // enum (`if c { E::A } else { E::B }`) still agree on the
                // enum itself, which is then the whole `if`'s type.
                let result_type = match &else_kind {
                    Some(k) => branch_kinds.iter().cloned().chain(std::iter::once(k.clone())).flatten().next(),
                    None => None,
                }
                .map(|t| t.widened())
                .unwrap_or(ResolvedType::Void);

                let mismatch = branch_kinds
                    .iter()
                    .cloned()
                    .chain(else_kind.iter().cloned())
                    .flatten()
                    .find(|t| !result_type.accepts(t));
                if let Some(found) = mismatch {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::IfBranchTypeMismatch { expected: result_type, found },
                    ));
                    return None;
                }

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: result_type,
                    kind: CheckedExpr::If(CheckedIf { branches: checked_branches, else_branch: checked_else }),
                })
            }

            HirExpr::FunctionCall(call) => {
                if let Some(result) = self.resolve_overloaded_call(node_id, span, call) {
                    return result;
                }
                if let Some(result) = self.resolve_overloaded_static_call(node_id, span, call) {
                    return result;
                }
                if let Some(result) = self.resolve_generic_call(node_id, span, call) {
                    return result;
                }

                let ResolvedCallee { callee, fn_type, implicit_self, checked_args } =
                    match self.resolve_callee(&call.callee, &call.args)? {
                        CalleeResolution::Dynamic(result) => return result,
                        CalleeResolution::Ordinary(resolved) => resolved,
                    };

                let mut args = Vec::with_capacity(call.args.len() + implicit_self.is_some() as usize);
                args.extend(implicit_self);

                match checked_args {
                    // Overload resolution already fully analyzed (and
                    // type-checked, including untyped-constant adaptation)
                    // every user-written argument itself, to score
                    // candidates -- redoing that here would risk
                    // double-erroring, and can't change the outcome anyway.
                    Some(overload_args) => args.extend(overload_args),
                    None => {
                        // The counts shown to the user exclude an implicit
                        // `self` (at this point `args` holds exactly that,
                        // and nothing else) -- the user never wrote it, so
                        // "takes 1 argument but 2 were supplied" for a 1-arg
                        // method call would only confuse.
                        let implicit_count = args.len();

                        for arg in &call.args {
                            let param_index = args.len();
                            if param_index >= fn_type.params.len() && !fn_type.is_variadic {
                                self.errors.push(AnalysisError::new(
                                    arg.id,
                                    arg.span,
                                    AnalysisErrorKind::WrongArgumentCount {
                                        expected: fn_type.params.len() - implicit_count,
                                        found: call.args.len(),
                                    },
                                ));
                                return None;
                            }

                            let expected_type =
                                (param_index < fn_type.params.len()).then(|| &fn_type.params[param_index].1);
                            let checked_arg = self.analyze_expr(arg, expected_type)?;
                            let checked_arg = self.coerce_to_expected(expected_type, checked_arg);

                            if let Some(expected_type) = expected_type {
                                if !expected_type.accepts(&checked_arg.r#type) {
                                    self.errors.push(AnalysisError::new(
                                        arg.id,
                                        arg.span,
                                        AnalysisErrorKind::ArgumentTypeMismatch {
                                            expected: expected_type.clone(),
                                            found: checked_arg.r#type.clone(),
                                        },
                                    ));
                                    return None;
                                }
                            }

                            args.push(checked_arg);
                        }
                    }
                }

                let return_type = *fn_type.return_type.clone();
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: return_type,
                    kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee), fn_type, args }),
                })
            }

            HirExpr::Assignment(assignment) => {
                let HirExpr::Place(place) = &assignment.target.expr else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::AssignmentTargetNotAPlace,
                    ));
                    return None;
                };
                let (checked_target, target_type, target_mutable) =
                    self.analyze_place(assignment.target.id, assignment.target.span, place, None)?;
                self.require_mutable_place(node_id, span, &place.root, &checked_target, target_mutable)?;

                // Resolved *before* the value, unlike almost everywhere else
                // in this match -- the target's own type is exactly the
                // expected type an unsuffixed literal value should adapt to.
                let checked_value = self.analyze_expr(&assignment.value, Some(&target_type))?;
                let checked_value = self.coerce_to_expected(Some(&target_type), checked_value);

                if !target_type.accepts(&checked_value.r#type) {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::AssignmentTypeMismatch {
                            target: target_type,
                            value: checked_value.r#type,
                        },
                    ));
                    return None;
                }

                // The tag and header fields are per-variant constants --
                // writable body fields are the only mutable part of an enum
                // value.
                if let Some(field) = Self::immutable_enum_member(&checked_target) {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::EnumFieldImmutable { field },
                    ));
                    return None;
                }

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: target_type,
                    kind: CheckedExpr::Assignment(CheckedAssignment {
                        target: checked_target,
                        value: Box::new(checked_value),
                    }),
                })
            }

            HirExpr::CompoundAssign(HirCompoundAssign { target, op, value }) => {
                self.analyze_compound_assign(node_id, span, target, *op, value)
            }

            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                // `&base[range]`/`&mut base[range]` -- a slice, not an
                // ordinary pointer; see `analyze_slice`'s own doc comment
                // for why this is the *only* way to produce one.
                if let HirExpr::Slice(HirSlice { base: slice_base, range }) = &base.expr {
                    return self.analyze_slice(node_id, span, slice_base, range, *mutable);
                }
                // `&[...]`/`&mut [...]` -- a compile-time slice, not an
                // ordinary place; see `analyze_const_slice`'s own doc
                // comment.
                if let HirExpr::ArrayLiteral(elements) = &base.expr {
                    return self.analyze_const_slice(node_id, span, elements, *mutable, expected);
                }
                let HirExpr::Place(place) = &base.expr else {
                    self.errors
                        .push(AnalysisError::new(node_id, span, AnalysisErrorKind::AddressOfNotAPlace));
                    return None;
                };
                let (checked_place, place_type, place_mutable) = self.analyze_place(base.id, base.span, place, None)?;

                let pointee_type = if *mutable {
                    // `&mut` requires write access, and -- unlike plain `&`
                    // below -- *always* produces a fully-widened pointee, no
                    // exception: the only way a mutable refined pointer can
                    // ever exist is a `match`-narrowed *view* of an
                    // already-mutable place, never something freshly minted
                    // here (see `ResolvedType::accepts`'s doc comment for why
                    // that distinction is what keeps a mutable pointer/slice
                    // from ever needing to widen implicitly).
                    self.require_mutable_place(node_id, span, &place.root, &checked_place, place_mutable)?;
                    // De-assumption: a writable alias to this place now
                    // exists, so any later direct read of it (in this or an
                    // enclosing scope) can no longer trust a narrower type
                    // than the plain one -- this is the actual "de-assume a
                    // proof once a mutable reference has been taken" step.
                    if let Some((ident, ..)) = self.narrowable_place(place) {
                        self.context.widen_variable(&ident);
                    }
                    place_type.widened()
                } else {
                    // A variant refinement surviving `&` is only sound when
                    // it's a *permanent* fact about the place -- its own
                    // declared/inferred type (`a := Entity::Person { ... }`;
                    // reassigning a different variant to `a` later is already
                    // rejected by `ResolvedType::accepts`, so a pointer into
                    // it can never go stale that way). A `match`-narrowed
                    // shadow's refinement is only true for that one arm's
                    // lexical scope -- the underlying storage can still hold
                    // a different variant once the arm ends -- so that case
                    // still widens, exactly as before this distinction
                    // existed (see `VarBinding::narrowed`).
                    let narrowed_shadow = self
                        .narrowable_place(place)
                        .is_some_and(|(ident, ..)| self.context.find_variable(&ident).is_some_and(|b| b.narrowed));
                    if narrowed_shadow { place_type.widened() } else { place_type }
                };

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Pointer { pointee: Box::new(pointee_type), mutable: *mutable },
                    kind: CheckedExpr::AddressOf(CheckedAddressOf { place: checked_place }),
                })
            }

            HirExpr::Negate(base) => {
                // `expected` passes straight through -- `Negate` is
                // transparent to its own result type (it's always exactly
                // `base`'s type, see below), so whatever type context this
                // node itself received is exactly the right context for
                // `base` too (including, notably, an unsuffixed literal
                // `base` -- `-100` is exactly as adaptable as `100`).
                let checked_base = self.analyze_expr(base, expected)?;
                // Signed ints and floats only -- matching Rust, unary `-` on
                // an unsigned integer (or `bool`/`char`, neither of which is
                // numeric at all) is rejected rather than silently wrapping.
                let negatable = matches!(
                    checked_base.r#type.numeric_kind(),
                    Some(NumericKind::Signed(_)) | Some(NumericKind::Float(_))
                );
                if !negatable {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::InvalidNegateOperand { r#type: checked_base.r#type },
                    ));
                    return None;
                }

                let r#type = checked_base.r#type.clone();
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type,
                    kind: CheckedExpr::Negate(Box::new(checked_base)),
                })
            }

            HirExpr::Increment(base) => self.analyze_incr_decr(node_id, span, base, BinaryOp::Add),
            HirExpr::Decrement(base) => self.analyze_incr_decr(node_id, span, base, BinaryOp::Sub),

            HirExpr::BinaryOp(bin) => {
                let checked_left = self.analyze_expr(&bin.left, None)?;
                let checked_right = self.analyze_expr(&bin.right, None)?;
                self.analyze_binary_op(node_id, span, bin.op, checked_left, checked_right)
            }

            HirExpr::BitNot(base) => {
                // `expected` passes straight through, same reasoning as
                // `Negate`'s arm just above -- `~` is transparent to its own
                // result type.
                let checked_base = self.analyze_expr(base, expected)?;
                let bitnotable =
                    matches!(checked_base.r#type.numeric_kind(), Some(NumericKind::Signed(_) | NumericKind::Unsigned(_)));
                if !bitnotable {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::InvalidBitNotOperand { r#type: checked_base.r#type },
                    ));
                    return None;
                }

                let r#type = checked_base.r#type.clone();
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type,
                    kind: CheckedExpr::BitNot(Box::new(checked_base)),
                })
            }

            HirExpr::ArrayLiteral(elements) => {
                let Some((first, rest)) = elements.split_first() else {
                    self.errors
                        .push(AnalysisError::new(node_id, span, AnalysisErrorKind::EmptyArrayLiteral));
                    return None;
                };

                // A declared/expected element type (from `[T; N]` context)
                // is used as *every* element's own expected type, including
                // the first -- unlike the plain bottom-up fallback below,
                // where only later elements are checked against the
                // first's own inferred type, never the other way around.
                let declared_item_type = match expected {
                    Some(ResolvedType::SizedArray(item_type, _)) => Some(item_type.as_ref()),
                    _ => None,
                };

                let checked_first = self.analyze_expr(first, declared_item_type)?;
                // Widened for the same reason an `if`'s branches are -- an
                // array of mixed variants of one enum is an array of that
                // enum.
                let item_type = declared_item_type.cloned().unwrap_or_else(|| checked_first.r#type.widened());

                let mut checked_elements = Vec::with_capacity(elements.len());
                let check_element = |this: &mut Self, id: HirId, elem_span: Span, checked: CheckedExprNode| {
                    if !item_type.accepts(&checked.r#type) {
                        this.errors.push(AnalysisError::new(
                            id,
                            elem_span,
                            AnalysisErrorKind::ArrayElementTypeMismatch {
                                expected: item_type.clone(),
                                found: checked.r#type.clone(),
                            },
                        ));
                        return None;
                    }
                    Some(checked)
                };
                checked_elements.push(check_element(self, first.id, first.span, checked_first)?);

                for element in rest {
                    let checked_element = self.analyze_expr(element, Some(&item_type))?;
                    checked_elements.push(check_element(self, element.id, element.span, checked_element)?);
                }

                let size = checked_elements.len() as u32;
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::SizedArray(Box::new(item_type.clone()), size),
                    kind: CheckedExpr::ArrayLiteral(CheckedArrayLiteral { item_type, elements: checked_elements }),
                })
            }

            HirExpr::StructLiteral(lit) => self.analyze_struct_literal(node_id, span, lit),

            // Reached only when *not* wrapped in `&`/`&mut` (see
            // `HirExpr::AddressOf`'s arm, which intercepts the `&`-wrapped
            // shape and calls `analyze_slice` directly) -- a slice
            // expression alone can't tell whether the user meant an
            // immutable or mutable slice, so it's never valid on its own.
            HirExpr::Slice(_) => {
                self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::SliceRequiresAddressOf));
                None
            }

            HirExpr::Match(m) => self.analyze_match(node_id, span, m),

            HirExpr::Cast(HirCast { target, base }) => {
                let target_type = self.resolve_type_or_error(node_id, span, target, true)?;
                // `base` keeps its own natural (default, unsuffixed-literal)
                // type -- the cast's target is an explicit instruction to
                // convert, never context to infer `base`'s own type from
                // (`<f32>10` casts a genuine i32 `10` to `f32`, it doesn't
                // just relabel an already-f32 literal).
                let checked_base = self.analyze_expr(base, None)?;

                if let (ResolvedType::Pointer { mutable: true, .. }, ResolvedType::Pointer { mutable: false, .. }) =
                    (&target_type, &checked_base.r#type)
                {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::CastToMutablePointer {
                            from: checked_base.r#type.clone(),
                            to: target_type.clone(),
                        },
                    ));
                    return None;
                }

                let (Some(source_class), Some(target_class)) =
                    (checked_base.r#type.cast_class(), target_type.cast_class())
                else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::InvalidCast { from: checked_base.r#type.clone(), to: target_type.clone() },
                    ));
                    return None;
                };

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: target_type.clone(),
                    kind: CheckedExpr::Cast(CheckedCast {
                        kind: Self::resolve_cast_kind(source_class, target_class),
                        target_type,
                        base: Box::new(checked_base),
                    }),
                })
            }
        }
    }

    /// Picks the one `CastKind` a `(source, target)` `CastClass` pair needs,
    /// purely from width/signedness -- no per-type-pair table (see
    /// `CastClass`'s doc comment).
    fn resolve_cast_kind(source: CastClass, target: CastClass) -> CastKind {
        match (source, target) {
            (CastClass::Int { width: sw, signed }, CastClass::Int { width: tw, .. }) => {
                if sw == tw {
                    CastKind::Reinterpret
                } else if sw < tw {
                    // Widening reproduces the *source's* value, so it's the
                    // source's signedness that picks sign- vs zero-extend
                    // (matches Rust's `as`: `-1i8 as u32 == u32::MAX`).
                    CastKind::IntExtend { signed }
                } else {
                    CastKind::IntTruncate
                }
            }
            (CastClass::Int { signed, .. }, CastClass::Float { .. }) => CastKind::IntToFloat { signed },
            (CastClass::Float { .. }, CastClass::Int { signed, .. }) => CastKind::FloatToInt { signed },
            (CastClass::Float { width: sw }, CastClass::Float { width: tw }) => {
                if sw == tw {
                    CastKind::Reinterpret
                } else if sw < tw {
                    CastKind::FloatExtend
                } else {
                    CastKind::FloatTruncate
                }
            }
        }
    }

    /// `match scrutinee { pattern => body, ... } else { ... }` -- both an
    /// exhaustive switch and the proof mechanism behind sum-type subtyping.
    /// For an enum scrutinee that is exactly a bare local/parameter
    /// reference (`narrowable_scrutinee`), each arm that names a specific
    /// variant re-declares that same binding, narrowed to that variant, for
    /// the duration of analyzing the arm's body -- ordinary lexical
    /// shadowing (`Context::enter_scope`/`declare_binding`), not a new
    /// mechanism; `Context::find_variable` already prefers the innermost
    /// scope, and shadowing an outer binding is already-supported ordinary
    /// scoping. `else` (when present) always analyzes against the
    /// un-narrowed type, matching the "still just a generic Entity" rule.
    ///
    /// Any other scrutinee shape (a field access, a deref, a plain
    /// non-place expression, ...) is still fully supported for branching,
    /// just without narrowing -- there's no name to narrow -- and is
    /// evaluated exactly once into a synthesized local first, so a
    /// side-effecting scrutinee expression (e.g. a function call) isn't
    /// silently re-run once per arm the way re-parsing the source
    /// expression per condition would.
    fn analyze_match(&mut self, node_id: HirId, span: Span, m: &HirMatch) -> Option<CheckedExprNode> {
        let narrow_target = self.narrowable_scrutinee(&m.scrutinee);
        let checked_scrutinee = self.analyze_expr(&m.scrutinee, None)?;
        let scrutinee_type = checked_scrutinee.r#type.clone();

        let (scrutinee_read, prelude_stmts, narrow_binding) = if let Some((ident, decl_id, storage, mutable)) = narrow_target {
            (checked_scrutinee.clone(), Vec::new(), Some((ident, decl_id, storage, mutable)))
        } else {
            let target = CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id: node_id, storage: Storage::Local, r#type: scrutinee_type.clone() },
                projections: vec![],
            };
            let decl = CheckedStmt::Declaration(CheckedDeclaration {
                id: node_id,
                span,
                ident: Ident("$scrutinee".to_string()),
                r#type: scrutinee_type.clone(),
            });
            let assign = CheckedStmt::Expression(CheckedExprNode {
                id: node_id,
                span,
                r#type: scrutinee_type.clone(),
                kind: CheckedExpr::Assignment(CheckedAssignment { target: target.clone(), value: Box::new(checked_scrutinee) }),
            });
            let read = CheckedExprNode { id: node_id, span, r#type: scrutinee_type.clone(), kind: CheckedExpr::Place(target) };
            (read, vec![decl, assign], None)
        };

        let is_enum_scrutinee = matches!(&scrutinee_type, ResolvedType::Enum { .. })
            || matches!(&scrutinee_type, ResolvedType::Pointer { pointee, .. } if matches!(**pointee, ResolvedType::Enum { .. }));

        let (arms, else_branch, result_type) = if is_enum_scrutinee {
            self.analyze_enum_match(node_id, span, m, &scrutinee_type, &scrutinee_read, narrow_binding)?
        } else if scrutinee_type.integer_domain().is_some() {
            self.analyze_value_match(node_id, span, m, &scrutinee_type, &scrutinee_read)?
        } else {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::UnsupportedMatchScrutinee { r#type: scrutinee_type },
            ));
            return None;
        };

        let checked_match = CheckedExprNode {
            id: node_id,
            span,
            r#type: result_type.clone(),
            kind: CheckedExpr::Match(CheckedMatch { arms, else_branch }),
        };

        if prelude_stmts.is_empty() {
            Some(checked_match)
        } else {
            Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: result_type,
                kind: CheckedExpr::Codeblock(CheckedBlock { stmts: prelude_stmts, tail: Some(Box::new(checked_match)) }),
            })
        }
    }

    /// A place that's exactly a bare local/parameter reference -- no
    /// projections, no module qualification, no explicit generic
    /// arguments. Only this shape supports narrowing/de-assumption (see
    /// `analyze_match`'s and `HirExpr::AddressOf`'s doc comments): its
    /// identity (`decl_id`/`storage`) is what gets shadow-declared (match
    /// narrowing) or widened in place (`Context::widen_variable`, `&mut`'s
    /// de-assumption).
    fn narrowable_place(&self, place: &HirPlace) -> Option<(Ident, HirId, Storage, bool)> {
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else { return None };
        if !expr_path.generic_args.is_empty() || !expr_path.path.is_unqualified() {
            return None;
        }
        let binding = self.context.find_variable(&expr_path.path.head)?;
        Some((expr_path.path.head.clone(), binding.decl_id, binding.storage, binding.mutable))
    }

    /// `narrowable_place`, but for a scrutinee expression rather than an
    /// already-unwrapped place -- `match`'s scrutinee is a full
    /// `HirExprNode` (it doesn't have to be a place at all, see
    /// `analyze_match`'s doc comment), so this just unwraps the one shape
    /// that's ever narrowable before delegating.
    fn narrowable_scrutinee(&self, scrutinee: &HirExprNode) -> Option<(Ident, HirId, Storage, bool)> {
        let HirExpr::Place(place) = &scrutinee.expr else { return None };
        self.narrowable_place(place)
    }

    /// An arm's body: a `{ ... }` block analyzes exactly like any other
    /// codeblock; a bare expression (`100 => "a hundred"`, no braces) is
    /// wrapped in a trivial one-tail-expression block, so codegen only ever
    /// deals with one shape (the same normalization `if`'s branches don't
    /// need, since `if` never allows a bare-expression branch body).
    fn analyze_match_arm_body(&mut self, body: &HirExprNode) -> Option<CheckedBlock> {
        if let HirExpr::Codeblock(block) = &body.expr {
            self.analyze_block(block, None)
        } else {
            let checked = self.analyze_expr(body, None)?;
            Some(CheckedBlock { stmts: vec![], tail: Some(Box::new(checked)) })
        }
    }

    /// The `analyze_match` case where `scrutinee_type` is `Enum{..}` or
    /// `Pointer(Enum{..})` -- through a pointer, a matched arm narrows the
    /// pointer's own pointee refinement (`*Entity` -> `*Entity::Person`),
    /// exactly like the plain-value case narrows the value's own type
    /// (see `analyze_match`'s doc comment).
    fn analyze_enum_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_read: &CheckedExprNode,
        narrow_binding: Option<(Ident, HirId, Storage, bool)>,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        // `through_pointer` is the scrutinee's own pointer mutability when
        // matching through one -- narrowing only ever refines the pointee,
        // never changes whether the pointer itself is writable.
        let (cell, through_pointer) = match scrutinee_type {
            ResolvedType::Enum { cell, .. } => (cell.clone(), None),
            ResolvedType::Pointer { pointee, mutable } => match &**pointee {
                ResolvedType::Enum { cell, .. } => (cell.clone(), Some(*mutable)),
                _ => unreachable!("caller already confirmed this is an enum or pointer-to-enum"),
            },
            _ => unreachable!("caller already confirmed this is an enum or pointer-to-enum"),
        };

        // `.tag` -- the same read regardless of which variant an arm
        // matches (every value of an enum carries one); built once here,
        // reused (cloned) per arm's condition.
        let mut tag_projections = Vec::new();
        let tag_type = self.resolve_field_projection(
            node_id,
            span,
            &mut tag_projections,
            scrutinee_type,
            &Ident("tag".to_string()),
            &mut false,
        )?;
        let CheckedExpr::Place(scrutinee_place) = &scrutinee_read.kind else {
            unreachable!("analyze_match always builds scrutinee_read as a place read")
        };
        let tag_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace { root: scrutinee_place.root.clone(), projections: tag_projections }),
        };

        let mut covered: HashMap<usize, Span> = HashMap::new();
        let mut checked_arms = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            let HirPattern::Value(pattern_expr) = &arm.pattern else {
                self.errors.push(AnalysisError::new(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::PatternNotEnumVariant { r#enum: cell.borrow().name.clone() },
                ));
                return None;
            };
            let variant_index = self.resolve_variant_pattern(&cell, pattern_expr)?;

            if let Some(previous) = covered.insert(variant_index, arm.pattern.span()) {
                self.errors.push(AnalysisError::new(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::OverlappingMatchArm { previous },
                ));
                return None;
            }

            let tag_const = CheckedExprNode {
                id: node_id,
                span: arm.pattern.span(),
                r#type: tag_type.clone(),
                kind: CheckedExpr::Number(cell.borrow().variants[variant_index].tag),
            };
            let condition = CheckedExprNode {
                id: node_id,
                span: arm.pattern.span(),
                r#type: ResolvedType::Bool,
                kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                    op: BinaryOp::Eq,
                    left: Box::new(tag_read.clone()),
                    right: Box::new(tag_const),
                }),
            };

            self.context.enter_scope();
            if let Some((ident, decl_id, storage, mutable)) = &narrow_binding {
                let refined = ResolvedType::Enum { cell: cell.clone(), variant: Some(variant_index) };
                let narrowed = match through_pointer {
                    Some(pointer_mutable) => ResolvedType::Pointer { pointee: Box::new(refined), mutable: pointer_mutable },
                    None => refined,
                };
                self.declare_narrowed_binding(*decl_id, arm.span, ident, narrowed, *storage, *mutable);
            }
            let body = self.analyze_match_arm_body(&arm.body);
            self.context.leave_scope();

            checked_arms.push(CheckedMatchArm { conditions: vec![condition], body: body? });
        }

        let variant_count = cell.borrow().variants.len();
        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if covered.len() < variant_count => {
                let missing: Vec<Ident> = cell
                    .borrow()
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| !covered.contains_key(idx))
                    .map(|(_, v)| v.name.clone())
                    .collect();
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchEnum { r#enum: cell.borrow().name.clone(), missing },
                ));
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    /// Resolves a match pattern that must name one of `cell`'s variants
    /// (`Entity::Person`) -- deliberately its own, simpler lookup rather
    /// than reusing `resolve_type_member`/`resolve_unit_variant`: those
    /// build a *construction* (rejecting a variant that has body fields,
    /// via `EnumVariantMissingBody`), which is wrong here -- a pattern only
    /// ever tests the tag, so a variant with a body is just as matchable as
    /// one without.
    fn resolve_variant_pattern(&mut self, cell: &Rc<RefCell<ResolvedEnumType>>, expr: &HirExprNode) -> Option<usize> {
        let shaped_as_variant_path = matches!(
            &expr.expr,
            HirExpr::Place(HirPlace { root: HirPlaceRoot::Path(p), projections })
                if projections.is_empty() && p.generic_args.is_empty() && !p.path.tail.is_empty()
        );
        if !shaped_as_variant_path {
            self.errors.push(AnalysisError::new(
                expr.id,
                expr.span,
                AnalysisErrorKind::PatternNotEnumVariant { r#enum: cell.borrow().name.clone() },
            ));
            return None;
        }
        let HirExpr::Place(HirPlace { root: HirPlaceRoot::Path(expr_path), .. }) = &expr.expr else {
            unreachable!("just confirmed above")
        };
        let variant_name = expr_path.path.tail.last().expect("just confirmed non-empty above");
        let enum_name_segment = if expr_path.path.tail.len() == 1 {
            &expr_path.path.head
        } else {
            &expr_path.path.tail[expr_path.path.tail.len() - 2]
        };
        if *enum_name_segment != cell.borrow().name {
            self.errors.push(AnalysisError::new(
                expr.id,
                expr.span,
                AnalysisErrorKind::PatternIsEnumVariant {
                    r#enum: enum_name_segment.clone(),
                    variant: variant_name.clone(),
                    scrutinee: ResolvedType::Enum { cell: cell.clone(), variant: None },
                },
            ));
            return None;
        }
        let found = cell.borrow().variant(variant_name).map(|(idx, _)| idx);
        match found {
            Some(idx) => Some(idx),
            None => {
                let similar = best_match(variant_name, cell.borrow().variants.iter().map(|v| &v.name));
                self.errors.push(AnalysisError::new(
                    expr.id,
                    expr.span,
                    AnalysisErrorKind::NoSuchVariantInPattern {
                        r#enum: cell.borrow().name.clone(),
                        name: variant_name.clone(),
                        similar,
                    },
                ));
                None
            }
        }
    }

    /// The `analyze_match` case where `scrutinee_type` is an integer type or
    /// `Bool` (`ResolvedType::integer_domain`'s `Some` cases) -- value and
    /// range patterns, checked for full-domain coverage by
    /// `crate::exhaustiveness`.
    fn analyze_value_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_read: &CheckedExprNode,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        let domain = scrutinee_type
            .integer_domain()
            .expect("caller already confirmed this type has an integer domain");

        let mut checked_arms = Vec::with_capacity(m.arms.len());
        let mut intervals = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            let (lo, hi, conditions) = self.analyze_value_pattern(&arm.pattern, scrutinee_type, scrutinee_read)?;
            intervals.push(crate::exhaustiveness::Interval { lo, hi, span: arm.pattern.span() });
            let body = self.analyze_match_arm_body(&arm.body)?;
            checked_arms.push(CheckedMatchArm { conditions, body });
        }

        let coverage = crate::exhaustiveness::check(domain, intervals);
        if !coverage.overlaps.is_empty() {
            for (previous, redundant) in &coverage.overlaps {
                self.errors.push(AnalysisError::new(
                    node_id,
                    redundant.span,
                    AnalysisErrorKind::OverlappingMatchArm { previous: previous.span },
                ));
            }
            return None;
        }

        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if !coverage.gaps.is_empty() => {
                let gaps = coverage.gaps.iter().map(|(lo, hi)| Self::describe_gap(scrutinee_type, *lo, *hi)).collect();
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchValue { r#type: scrutinee_type.clone(), gaps },
                ));
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    /// One value/range pattern's covered interval, plus the runtime
    /// condition(s) that test it -- one condition per bound actually
    /// present (a range pattern's absent bound needs no runtime check at
    /// all, since it's already the domain's own edge).
    fn analyze_value_pattern(
        &mut self,
        pattern: &HirPattern,
        scrutinee_type: &ResolvedType,
        scrutinee_read: &CheckedExprNode,
    ) -> Option<(i128, i128, Vec<CheckedExprNode>)> {
        match pattern {
            HirPattern::Value(expr) => {
                let value = self.const_eval_pattern(expr, scrutinee_type)?;
                let n = Self::const_value_as_i128(&value);
                let condition = Self::value_cmp_condition(scrutinee_read, expr.id, expr.span, scrutinee_type, BinaryOp::Eq, value);
                Some((n, n, vec![condition]))
            }
            HirPattern::Range(range) => {
                let domain = scrutinee_type.integer_domain().expect("caller already confirmed an integer domain");
                let mut conditions = Vec::new();
                let lo = match &range.start {
                    Some(e) => {
                        let value = self.const_eval_pattern(e, scrutinee_type)?;
                        let n = Self::const_value_as_i128(&value);
                        conditions.push(Self::value_cmp_condition(scrutinee_read, e.id, e.span, scrutinee_type, BinaryOp::Ge, value));
                        n
                    }
                    None => domain.0,
                };
                let hi = match &range.end {
                    Some(e) => {
                        let value = self.const_eval_pattern(e, scrutinee_type)?;
                        let n = Self::const_value_as_i128(&value);
                        let op = if range.inclusive { BinaryOp::Le } else { BinaryOp::Lt };
                        conditions.push(Self::value_cmp_condition(scrutinee_read, e.id, e.span, scrutinee_type, op, value));
                        if range.inclusive { n } else { n - 1 }
                    }
                    None => domain.1,
                };
                Some((lo, hi, conditions))
            }
        }
    }

    /// The pattern-position sibling of `const_eval`: a literal constant
    /// (a number, optionally negated, or a bool), checked against
    /// `expected` -- the scrutinee's own exact type drives interpretation,
    /// same reasoning as `const_eval`. Deliberately its own function rather
    /// than reusing `const_eval` outright: that function's fallback error
    /// (`EnumValueNotConstant`) is worded specifically for enum header
    /// values, which would be a confusing thing to say about a match
    /// pattern; `const_number` itself (the actual parsing/range-checking
    /// logic) is fully shared.
    fn const_eval_pattern(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number),
                _ => {
                    self.errors.push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::PatternValueNotConstant));
                    None
                }
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => {
                    self.errors.push(AnalysisError::new(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::PatternTypeMismatch { expected: expected.clone(), found: ResolvedType::Bool },
                    ));
                    None
                }
            },
            _ => {
                self.errors.push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::PatternValueNotConstant));
                None
            }
        }
    }

    fn const_value_as_i128(value: &ConstValue) -> i128 {
        match value {
            ConstValue::Number(NumberValue::Signed(n)) => *n as i128,
            ConstValue::Number(NumberValue::Unsigned(n)) => *n as i128,
            ConstValue::Number(NumberValue::Float(_)) => {
                unreachable!("match patterns are never float-typed -- integer_domain excludes floats")
            }
            ConstValue::Bool(b) => *b as i128,
            ConstValue::Char(_) | ConstValue::Str(_) | ConstValue::Slice(_) | ConstValue::Array(_) => {
                unreachable!("analyze_value_match only ever runs for an integer/bool scrutinee type")
            }
        }
    }

    fn value_cmp_condition(
        scrutinee_read: &CheckedExprNode,
        id: HirId,
        span: Span,
        scrutinee_type: &ResolvedType,
        op: BinaryOp,
        value: ConstValue,
    ) -> CheckedExprNode {
        let kind = match value {
            ConstValue::Number(n) => CheckedExpr::Number(n),
            ConstValue::Bool(b) => CheckedExpr::Bool(b),
            ConstValue::Char(_) | ConstValue::Str(_) | ConstValue::Slice(_) | ConstValue::Array(_) => {
                unreachable!("analyze_value_match only ever runs for an integer/bool scrutinee type")
            }
        };
        let constant = CheckedExprNode { id, span, r#type: scrutinee_type.clone(), kind };
        CheckedExprNode {
            id,
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(scrutinee_read.clone()), right: Box::new(constant) }),
        }
    }

    /// A gap's inclusive `[lo, hi]` bounds, formatted for
    /// `NonExhaustiveMatchValue`'s diagnostic -- `bool`'s domain renders as
    /// `true`/`false` rather than `0`/`1`, since that's how a `bool`
    /// pattern is actually written.
    fn describe_gap(scrutinee_type: &ResolvedType, lo: i128, hi: i128) -> String {
        let render = |n: i128| {
            if *scrutinee_type == ResolvedType::Bool { if n == 0 { "false".to_string() } else { "true".to_string() } } else { n.to_string() }
        };
        if lo == hi { render(lo) } else { format!("{}..={}", render(lo), render(hi)) }
    }

    /// Unifies every arm's (and `else`'s, if present) resolved type exactly
    /// like `HirExpr::If` does (see that arm's own comments) -- first
    /// concrete (non-diverging) type, widened, checked against every other
    /// arm via `accepts`. Unlike `if`, an absent `else` never forces `Void`:
    /// by the time this runs, `analyze_enum_match`/`analyze_value_match`
    /// have already guaranteed the arms are exhaustive on their own, so a
    /// real value always comes from some arm.
    fn unify_match_arm_types(
        &mut self,
        node_id: HirId,
        span: Span,
        arms: &[CheckedMatchArm],
        else_branch: &Option<CheckedBlock>,
    ) -> Option<ResolvedType> {
        let arm_kinds: Vec<Option<ResolvedType>> = arms.iter().map(|a| Self::block_type(&a.body)).collect();
        let else_kind: Option<Option<ResolvedType>> = else_branch.as_ref().map(Self::block_type);

        let result_type = arm_kinds
            .iter()
            .cloned()
            .chain(else_kind.iter().cloned())
            .flatten()
            .next()
            .map(|t| t.widened())
            .unwrap_or(ResolvedType::Void);

        let mismatch = arm_kinds.into_iter().chain(else_kind).flatten().find(|t| !result_type.accepts(t));
        if let Some(found) = mismatch {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::MatchArmTypeMismatch { expected: result_type, found },
            ));
            return None;
        }
        Some(result_type)
    }

    /// `Name { field = value; ... }` -- builds a whole struct value, or --
    /// when the path names an enum variant (`Enum::Variant { ... }`) -- a
    /// whole enum value, in one expression. The literal's name resolves
    /// with the same diagnostics (typo suggestions included) type positions
    /// already give (see `resolve_literal_target`), and every declared
    /// field must be set exactly once with a value of its exact type. All
    /// field problems in one literal are reported in one pass (same
    /// keep-going discipline as `analyze_all`), not just the first.
    fn analyze_struct_literal(
        &mut self,
        node_id: HirId,
        span: Span,
        lit: &HirStructLiteral,
    ) -> Option<CheckedExprNode> {
        match self.resolve_literal_target(node_id, span, &lit.path)? {
            LiteralTarget::Struct(resolved) => {
                let ResolvedType::Struct(cell) = &resolved else {
                    unreachable!("LiteralTarget::Struct always wraps ResolvedType::Struct");
                };
                // Snapshot the declared fields so `cell` isn't borrowed
                // across the value analysis below -- a nested literal of
                // this same struct type needs to borrow it again.
                let declared: Vec<(Ident, ResolvedType)> = cell.borrow().fields.clone();
                let struct_name = cell.borrow().name.clone();
                let base = resolved.clone();
                let fields = self.check_field_initializers(
                    node_id,
                    span,
                    &struct_name,
                    &declared,
                    &lit.fields,
                    |field| AnalysisErrorKind::NoSuchField { field: field.name.clone(), base: base.clone() },
                )?;
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: resolved,
                    kind: CheckedExpr::StructLiteral(CheckedStructLiteral { fields }),
                })
            }
            LiteralTarget::EnumVariant(cell, variant_index) => {
                let (enum_name, variant_name, declared, header_names) = {
                    let e = cell.borrow();
                    let v = &e.variants[variant_index];
                    let header_names: Vec<Ident> = e.header.iter().map(|(name, _)| name.clone()).collect();
                    // Shared dynamic fields first (declaration order), then
                    // this variant's own body fields -- every construction
                    // site must supply both, in one combined literal.
                    let declared: Vec<(Ident, ResolvedType)> =
                        e.dynamic_fields.iter().chain(v.fields.iter()).cloned().collect();
                    (e.name.clone(), v.name.clone(), declared, header_names)
                };
                if declared.is_empty() {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::EnumVariantHasNoBody { r#enum: enum_name, variant: variant_name },
                    ));
                    return None;
                }
                let declared_names: Vec<Ident> = declared.iter().map(|(name, _)| name.clone()).collect();
                let unknown_enum = enum_name.clone();
                let fields = self.check_field_initializers(
                    node_id,
                    span,
                    &variant_name,
                    &declared,
                    &lit.fields,
                    move |field| {
                        if field.name.as_ref() == "tag" || header_names.contains(&field.name) {
                            AnalysisErrorKind::EnumHeaderFieldInLiteral { field: field.name.clone() }
                        } else {
                            AnalysisErrorKind::NoSuchEnumField {
                                field: field.name.clone(),
                                r#enum: unknown_enum.clone(),
                                similar: best_match(&field.name, declared_names.iter()),
                            }
                        }
                    },
                )?;
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Enum { cell, variant: Some(variant_index) },
                    kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct { variant_index, fields }),
                })
            }
            LiteralTarget::Union(resolved) => {
                let ResolvedType::Union(cell) = &resolved else {
                    unreachable!("LiteralTarget::Union always wraps ResolvedType::Union");
                };
                let declared: Vec<(Ident, ResolvedType)> = cell.borrow().fields.clone();
                let union_name = cell.borrow().name.clone();

                if lit.fields.is_empty() {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnionLiteralMissingField { r#union: union_name },
                    ));
                    return None;
                }
                if lit.fields.len() > 1 {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnionLiteralTooManyFields {
                            r#union: union_name,
                            fields: lit.fields.iter().map(|f| f.name.clone()).collect(),
                        },
                    ));
                    return None;
                }

                let field = &lit.fields[0];
                let found = declared
                    .iter()
                    .enumerate()
                    .find(|(_, (name, _))| name == &field.name)
                    .map(|(index, (_, r#type))| (index, r#type.clone()));
                let Some((field_index, expected)) = found else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        field.name_span,
                        AnalysisErrorKind::NoSuchField { field: field.name.clone(), base: resolved.clone() },
                    ));
                    return None;
                };
                let value = self.analyze_expr(&field.value, Some(&expected))?;
                if !expected.accepts(&value.r#type) {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        value.span,
                        AnalysisErrorKind::FieldTypeMismatch {
                            field: field.name.clone(),
                            expected,
                            found: value.r#type.clone(),
                        },
                    ));
                    return None;
                }

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: resolved,
                    kind: CheckedExpr::UnionConstruct(CheckedUnionConstruct { field_index, value: Box::new(value) }),
                })
            }
        }
    }

    /// The shared per-field discipline behind both literal forms: each
    /// initializer must name a declared field, exactly once, with a value
    /// of its field's type; every declared field must be covered (there is
    /// no implicit zeroing). `unknown_field` supplies the form-specific
    /// "no such field" diagnostic. `owner` is the name shown by the
    /// missing-fields error (the struct, or the enum variant).
    fn check_field_initializers(
        &mut self,
        node_id: HirId,
        span: Span,
        owner: &Ident,
        declared: &[(Ident, ResolvedType)],
        fields: &[omega_hir::HirStructLiteralField],
        unknown_field: impl Fn(&omega_hir::HirStructLiteralField) -> AnalysisErrorKind,
    ) -> Option<Vec<CheckedStructLiteralField>> {
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        let mut checked_fields = Vec::with_capacity(fields.len());
        let mut ok = true;
        for field in fields {
            if let Some(previous) = seen.insert(field.name.clone(), field.name_span) {
                self.errors.push(AnalysisError::new(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::DuplicateFieldInitializer { field: field.name.clone(), previous },
                ));
                ok = false;
                continue;
            }
            let found = declared
                .iter()
                .enumerate()
                .find(|(_, (name, _))| name == &field.name)
                .map(|(index, (_, r#type))| (index, r#type.clone()));
            let Some((field_index, expected)) = found else {
                self.errors.push(AnalysisError::new(node_id, field.name_span, unknown_field(field)));
                ok = false;
                continue;
            };
            let Some(value) = self.analyze_expr(&field.value, Some(&expected)) else {
                ok = false;
                continue;
            };
            if !expected.accepts(&value.r#type) {
                self.errors.push(AnalysisError::new(
                    node_id,
                    value.span,
                    AnalysisErrorKind::FieldTypeMismatch {
                        field: field.name.clone(),
                        expected,
                        found: value.r#type.clone(),
                    },
                ));
                ok = false;
                continue;
            }
            checked_fields.push(CheckedStructLiteralField { field_index, value });
        }

        let missing: Vec<Ident> =
            declared.iter().map(|(name, _)| name).filter(|name| !seen.contains_key(name)).cloned().collect();
        if !missing.is_empty() {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::MissingFieldInitializers { r#struct: owner.clone(), missing },
            ));
            ok = false;
        }

        ok.then_some(checked_fields)
    }

    /// What a `Name { ... }` literal's path actually names -- a struct, or
    /// one specific variant of an enum -- with the most precise error this
    /// can determine otherwise. Resolution order mirrors place-root
    /// resolution: explicit generic arguments pin the type prefix
    /// exactly; otherwise an imported-module alias reading of the head wins
    /// (trying the whole path as the type first, then all-but-last as an
    /// enum with the last segment its variant), and a non-alias multi-
    /// segment head must itself be a type in scope or this module's own.
    fn resolve_literal_target(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &ExprPath,
    ) -> Option<LiteralTarget> {
        if path.plain().is_none() {
            let segments = path.path.segments();
            let rest = segments[path.args_at + 1..].to_vec();
            if rest.len() > 1 {
                self.errors.push(AnalysisError::new(
                    node_id,
                    span,
                    AnalysisErrorKind::GenericPathTooDeep { r#type: segments[path.args_at].clone() },
                ));
                return None;
            }
            let type_args = self.resolve_generic_arg_list(node_id, span, path)?;
            let absolute = self.generic_prefix_absolute(node_id, span, &segments[..=path.args_at])?;
            let resolved = match self.resolver.resolve_item(&absolute, &type_args, true) {
                Ok(ResolvedItem::Type(t)) => t,
                Ok(ResolvedItem::Value { .. }) => {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(crate::error::TypeResolutionError::NotAType(absolute)),
                    ));
                    return None;
                }
                Err(e) => {
                    self.errors
                        .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                    return None;
                }
            };
            return self.literal_target_from_type(node_id, span, resolved, &rest);
        }

        let plain = &path.path;

        // A bare name: exactly a written type annotation, same diagnostics
        // (typo suggestions included) and all.
        if plain.is_unqualified() {
            let resolved = self.resolve_type_or_error(node_id, span, &Type::Named(plain.clone()), true)?;
            return self.literal_target_from_type(node_id, span, resolved, &[]);
        }

        // Module-qualified head: the whole path as the type first
        // (`mymodule::Vec2 { ... }`), then all-but-last as an enum whose
        // last segment names the variant (`mymodule::Shape::Circle`).
        let alias = self.resolve_alias_or_error(node_id, span, &plain.head)?;
        if let Some(ImportTarget::Module(target)) = &alias {
            let absolute: Vec<Ident> = target.iter().cloned().chain(plain.tail.iter().cloned()).collect();
            let first_error = match self.resolver.resolve_item(&absolute, &[], true) {
                Ok(ResolvedItem::Type(t)) => return self.literal_target_from_type(node_id, span, t, &[]),
                Ok(ResolvedItem::Value { .. }) => {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(crate::error::TypeResolutionError::NotAType(absolute)),
                    ));
                    return None;
                }
                Err(e) => e,
            };
            if absolute.len() >= 3 {
                let (variant, prefix) = absolute.split_last().expect("length checked above");
                if let Ok(ResolvedItem::Type(t)) = self.resolver.resolve_item(prefix, &[], true) {
                    return self.literal_target_from_type(node_id, span, t, std::slice::from_ref(variant));
                }
            }
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(first_error)));
            return None;
        }

        // Head isn't a module alias -- it must be a type (`Enum::Variant`):
        // local/imported first, then this module's own item, mirroring
        // `resolve_type_qualified_value`'s priority and error precision.
        if let Some(head_type) = self.context.find_defined_type(&plain.head).cloned() {
            return self.literal_target_from_type(node_id, span, head_type, &plain.tail);
        }
        if let Some(ImportTarget::Item(ResolvedItem::Type(t))) = alias {
            return self.literal_target_from_type(node_id, span, t, &plain.tail);
        }
        let absolute: Vec<Ident> = match alias {
            Some(ImportTarget::GenericItem(absolute)) => absolute,
            _ => self.module_path.iter().cloned().chain(std::iter::once(plain.head.clone())).collect(),
        };
        let kind = match self.resolver.resolve_item(&absolute, &[], true) {
            Ok(ResolvedItem::Type(t)) => {
                return self.literal_target_from_type(node_id, span, t, &plain.tail);
            }
            Ok(ResolvedItem::Value { .. }) => AnalysisErrorKind::NotAModule { name: plain.head.clone() },
            Err(ResolveError::UnknownItem { .. }) => AnalysisErrorKind::UndefinedPathHead {
                name: plain.head.clone(),
                similar_module: self.similar_import_alias(&plain.head),
                similar_type: self.context.similar_type_name(&plain.head).or_else(|| {
                    self.resolver.similar_item_name(&self.module_path, &plain.head, ItemNamespace::Type)
                }),
            },
            Err(e) => AnalysisErrorKind::ModuleResolution(e),
        };
        self.errors.push(AnalysisError::new(node_id, span, kind));
        None
    }

    /// Interprets an already-resolved type (plus at most one trailing path
    /// segment) as a literal's target -- see `resolve_literal_target`.
    fn literal_target_from_type(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: ResolvedType,
        rest: &[Ident],
    ) -> Option<LiteralTarget> {
        let kind = match &r#type {
            ResolvedType::Struct(cell) => match rest.first() {
                None => return Some(LiteralTarget::Struct(r#type.clone())),
                Some(name) => AnalysisErrorKind::StructLiteralPathTooDeep {
                    r#struct: cell.borrow().name.clone(),
                    name: name.clone(),
                },
            },
            ResolvedType::Union(cell) => match rest.first() {
                None => return Some(LiteralTarget::Union(r#type.clone())),
                Some(name) => AnalysisErrorKind::StructLiteralPathTooDeep {
                    r#struct: cell.borrow().name.clone(),
                    name: name.clone(),
                },
            },
            ResolvedType::Enum { cell, .. } => match rest {
                [] => {
                    let e = cell.borrow();
                    AnalysisErrorKind::EnumLiteralWithoutVariant {
                        r#enum: e.name.clone(),
                        example: e
                            .variants
                            .first()
                            .map(|v| v.name.clone())
                            .unwrap_or_else(|| Ident("Variant".into())),
                    }
                }
                [variant_name] => {
                    let found = cell.borrow().variant(variant_name).map(|(index, _)| index);
                    match found {
                        Some(index) => return Some(LiteralTarget::EnumVariant(cell.clone(), index)),
                        None => {
                            let e = cell.borrow();
                            AnalysisErrorKind::NoSuchEnumMember {
                                r#enum: e.name.clone(),
                                name: variant_name.clone(),
                                similar_variant: best_match(variant_name, e.variants.iter().map(|v| &v.name)),
                                similar_function: best_match(variant_name, e.functions.iter().map(|(name, _)| name)),
                            }
                        }
                    }
                }
                _ => AnalysisErrorKind::GenericPathTooDeep { r#type: cell.borrow().name.clone() },
            },
            _ if rest.is_empty() => AnalysisErrorKind::StructLiteralNotAStruct { found: r#type.clone() },
            _ => AnalysisErrorKind::StaticAccessOnNonStruct { found: r#type.clone() },
        };
        self.errors.push(AnalysisError::new(node_id, span, kind));
        None
    }

    /// `ident : Type = value;` -- builds the declaration and its
    /// initializing write by hand, exactly like `analyze_walrus` does,
    /// specifically so this write never goes through the ordinary
    /// `HirExpr::Assignment` arm's mutability check: it's the declaration's
    /// own initializer, never a `mut`-requiring reassignment, regardless of
    /// whether `ident` was declared `mut`.
    fn analyze_declaration_with_init(&mut self, decl: &HirDeclaration, value: &HirExprNode) -> Option<[CheckedStmt; 2]> {
        let checked_decl = self.analyze_declaration(decl, Storage::Local)?;
        let checked_value = self.analyze_expr(value, Some(&checked_decl.r#type))?;
        let checked_value = self.coerce_to_expected(Some(&checked_decl.r#type), checked_value);

        if !checked_decl.r#type.accepts(&checked_value.r#type) {
            self.errors.push(AnalysisError::new(
                value.id,
                value.span,
                AnalysisErrorKind::AssignmentTypeMismatch {
                    target: checked_decl.r#type.clone(),
                    value: checked_value.r#type.clone(),
                },
            ));
            return None;
        }

        let declaration = CheckedStmt::Declaration(checked_decl.clone());
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: decl.id,
            span: decl.span,
            r#type: checked_decl.r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: decl.id,
                        storage: Storage::Local,
                        r#type: checked_decl.r#type,
                    },
                    projections: vec![],
                },
                value: Box::new(checked_value),
            }),
        });

        Some([declaration, assignment])
    }

    /// Desugars `ident := value;` into the same two `CheckedStmt`s writing
    /// `ident : <inferred type>; ident = value;` by hand would produce --
    /// analysis is the only place that can do this desugaring, since only it
    /// knows `value`'s resolved type (there's nothing written down to carry
    /// a type otherwise). `value` is analyzed exactly once and reused as the
    /// assignment's value, rather than re-analyzed, to avoid double-reporting
    /// any error inside it.
    fn analyze_walrus(&mut self, w: &HirWalrusDeclaration) -> Option<[CheckedStmt; 2]> {
        let checked_value = self.analyze_expr(&w.value, None)?;
        let r#type = checked_value.r#type.clone();
        self.declare_binding(w.id, w.span, &w.ident, r#type.clone(), Storage::Local, w.mutable)?;

        let declaration = CheckedStmt::Declaration(CheckedDeclaration {
            id: w.id,
            span: w.span,
            ident: w.ident.clone(),
            r#type: r#type.clone(),
        });
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: w.id,
            span: w.span,
            r#type: r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable { decl_id: w.id, storage: Storage::Local, r#type },
                    projections: vec![],
                },
                value: Box::new(checked_value),
            }),
        });

        Some([declaration, assignment])
    }

    /// Most statements analyze into exactly one `CheckedStmt`; a walrus
    /// declaration desugars into two (see `analyze_walrus`), which is why
    /// this returns a `Vec` rather than routing through the 1-to-1
    /// `analyze_all` fold.
    fn analyze_stmt(&mut self, stmt: &HirStmt) -> Option<Vec<CheckedStmt>> {
        match stmt {
            HirStmt::Declaration(decl) => {
                self.analyze_declaration(decl, Storage::Local).map(|d| vec![CheckedStmt::Declaration(d)])
            }
            HirStmt::DeclarationWithInit(decl, value) => {
                self.analyze_declaration_with_init(decl, value).map(Vec::from)
            }
            HirStmt::ExternDeclaration(decl) => {
                self.analyze_extern_decl(decl).map(|d| vec![CheckedStmt::ExternDeclaration(d)])
            }
            HirStmt::Expression(expr) => self.analyze_expr(expr, None).map(|e| vec![CheckedStmt::Expression(e)]),
            HirStmt::Return(expr) => {
                if self.in_defer_body {
                    self.errors.push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::ReturnInsideDefer));
                    return None;
                }
                let return_type = self.current_return_type.clone();
                let checked = self.analyze_expr(expr, Some(&return_type))?;
                let checked = self.coerce_to_expected(Some(&return_type), checked);
                if !self.current_return_type.accepts(&checked.r#type) {
                    self.errors.push(AnalysisError::new(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::ReturnTypeMismatch {
                            expected: self.current_return_type.clone(),
                            found: checked.r#type.clone(),
                        },
                    ));
                    return None;
                }
                Some(vec![CheckedStmt::Return(checked)])
            }
            HirStmt::WalrusDeclaration(w) => self.analyze_walrus(w).map(Vec::from),
            HirStmt::While(w) => {
                let checked_cond = self.analyze_expr(&w.condition, None)?;
                if checked_cond.r#type != ResolvedType::Bool {
                    self.errors.push(AnalysisError::new(
                        w.id,
                        checked_cond.span,
                        AnalysisErrorKind::NonBoolCondition { r#type: checked_cond.r#type },
                    ));
                    return None;
                }
                self.loop_stack.push(w.id);
                let checked_body = self.analyze_block(&w.body, None);
                self.loop_stack.pop();
                let checked_body = checked_body?;
                Some(vec![CheckedStmt::While(CheckedWhile {
                    id: w.id,
                    span: w.span,
                    condition: checked_cond,
                    body: checked_body,
                })])
            }
            HirStmt::For(f) => self.analyze_for(f),
            HirStmt::Break(b) => match self.loop_stack.last() {
                Some(&loop_id) => Some(vec![CheckedStmt::Break(CheckedBreak { id: b.id, span: b.span, loop_id })]),
                None => {
                    self.errors.push(AnalysisError::new(b.id, b.span, AnalysisErrorKind::BreakOutsideLoop));
                    None
                }
            },
            HirStmt::Continue(c) => match self.loop_stack.last() {
                Some(&loop_id) => {
                    Some(vec![CheckedStmt::Continue(CheckedContinue { id: c.id, span: c.span, loop_id })])
                }
                None => {
                    self.errors.push(AnalysisError::new(c.id, c.span, AnalysisErrorKind::ContinueOutsideLoop));
                    None
                }
            },
            HirStmt::Defer(d) => {
                if !self.loop_stack.is_empty() {
                    self.errors.push(AnalysisError::new(d.id, d.span, AnalysisErrorKind::DeferInsideLoopNotSupported));
                    return None;
                }
                if self.in_defer_body {
                    self.errors.push(AnalysisError::new(d.id, d.span, AnalysisErrorKind::NestedDeferNotSupported));
                    return None;
                }
                let previous_in_defer_body = std::mem::replace(&mut self.in_defer_body, true);
                let body = self.analyze_block(&d.body, None);
                self.in_defer_body = previous_in_defer_body;
                let body = body?;
                Some(vec![CheckedStmt::Defer(CheckedDefer { id: d.id, span: d.span, body })])
            }
        }
    }

    /// `for`'s init/condition/post/body all share one scope of their own
    /// (so an `i := 0` init clause is visible to the condition/post/body
    /// but doesn't leak past the loop) -- entered once here, around all
    /// four, rather than the body getting its own additional nested scope
    /// from `analyze_block` alone. Every sub-part is still analyzed even
    /// after an earlier one fails (same "keep going, report everything"
    /// discipline as `analyze_all`), and the scope is always left before
    /// any early return.
    fn analyze_for(&mut self, f: &HirFor) -> Option<Vec<CheckedStmt>> {
        self.context.enter_scope();

        let mut ok = true;

        let checked_init = self.analyze_stmts(&f.init);
        ok &= checked_init.is_some();

        let checked_condition = match &f.condition {
            Some(c) => match self.analyze_expr(c, None) {
                Some(cc) if cc.r#type != ResolvedType::Bool => {
                    self.errors.push(AnalysisError::new(
                        f.id,
                        cc.span,
                        AnalysisErrorKind::NonBoolCondition { r#type: cc.r#type },
                    ));
                    ok = false;
                    None
                }
                Some(cc) => Some(cc),
                None => {
                    ok = false;
                    None
                }
            },
            None => {
                self.errors
                    .push(AnalysisError::new(f.id, f.span, AnalysisErrorKind::ForLoopMissingCondition));
                ok = false;
                None
            }
        };

        let checked_post = match &f.post {
            Some(p) => {
                let checked = self.analyze_expr(p, None);
                ok &= checked.is_some();
                checked
            }
            None => None,
        };

        self.loop_stack.push(f.id);
        let checked_body = self.analyze_block(&f.body, None);
        self.loop_stack.pop();
        ok &= checked_body.is_some();

        self.context.leave_scope();

        if !ok {
            return None;
        }

        Some(vec![CheckedStmt::For(Box::new(CheckedFor {
            id: f.id,
            span: f.span,
            init: checked_init?,
            condition: checked_condition?,
            post: checked_post,
            body: checked_body?,
        }))])
    }

    fn analyze_stmts(&mut self, stmts: &[HirStmt]) -> Option<Vec<CheckedStmt>> {
        let mut checked = Vec::with_capacity(stmts.len());
        let mut ok = true;
        for stmt in stmts {
            match self.analyze_stmt(stmt) {
                Some(mut items) => checked.append(&mut items),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        Some(self.truncate_unreachable(checked))
    }

    /// A function's declared return type must match its body's effective
    /// type (see `block_type`) -- a tail expression of the right type, or an
    /// unconditional trailing `return` (already individually type-checked
    /// against `current_return_type` when it was analyzed, so nothing more
    /// to check there), or (only for `Void`) falling off the end with no
    /// tail at all.
    fn check_function_return(
        &mut self,
        id: HirId,
        span: Span,
        return_type: &ResolvedType,
        body: &CheckedBlock,
    ) -> Option<()> {
        match Self::block_type(body) {
            None => Some(()),
            Some(found) if return_type.accepts(&found) => Some(()),
            Some(found) => {
                self.errors.push(AnalysisError::new(
                    id,
                    span,
                    AnalysisErrorKind::ReturnTypeMismatch { expected: return_type.clone(), found },
                ));
                None
            }
        }
    }

    /// A function's *signature* only: param and return types, with no scope
    /// entered and no param bound by name -- binding is a body-analysis-time
    /// concern (nothing needs to call a param by name yet), so this is
    /// strictly less work than `check_function_body`, not a restricted
    /// version of it. Registers the function's own name in the current
    /// (throwaway) scope too -- inert for a top-level function (nothing else
    /// ever looks at this particular `Context` again; `omega_driver::Driver`
    /// reads the *return value*, not this binding), but this same method
    /// also runs once per sibling method inside `signature_of_struct`'s
    /// method loop, where it *does* matter: it's what catches two methods
    /// sharing a name on one struct.
    /// Note this deliberately does *not* declare `f.name` into
    /// `self.context`'s current scope the way most other signature
    /// collection does -- when this runs once per sibling inside
    /// `signature_of_struct`/`enum`/`union`'s method loop, that binding
    /// would never actually be visible to anything (body-checking runs
    /// later, through an entirely separate `Analyzer`/`Context`; see
    /// `omega_driver::Driver::check_item_body`), so its *only* real effect
    /// was catching two methods sharing a name -- which up to two
    /// *overloaded* methods are now allowed to do (see
    /// `check_overload_duplicates`, called by each of those three methods
    /// once every sibling's signature is known). A top-level (non-method)
    /// caller never had a meaningful use for the binding either -- it
    /// always got a fresh, empty `Context`, so nothing could ever collide.
    pub fn collect_function_signature(&mut self, f: &HirFunctionDef) -> Option<ResolvedFunctionType> {
        // Param/return types are a function's signature, never inline data --
        // always indirect (see `analyze_param`'s identical reasoning).
        let params = self.analyze_all(&f.params, |this, p| {
            this.resolve_type_or_error(p.id, p.span, &p.r#type, true).map(|t| (p.ident.clone(), t))
        })?;
        let return_type = self.resolve_type_or_error(f.id, f.span, &f.return_type, true)?;
        Some(ResolvedFunctionType {
            params,
            return_type: Box::new(return_type),
            is_variadic: false,
            is_member_function: f.is_member_function,
        })
    }

    /// Compares every pair of `functions`' signatures by param-type list,
    /// ignoring parameter names -- the method-loop counterpart to
    /// `omega_driver::Driver::check_overload_duplicates` (see its doc
    /// comment for the full reasoning): two methods sharing a name is a
    /// valid overload as long as their signatures genuinely differ; an
    /// identical pair is a real duplicate, reported the same way a plain
    /// same-name collision always has been.
    fn check_overload_duplicates(&mut self, functions: &[HirFunctionDef], signatures: &[ResolvedFunctionType]) {
        for i in 1..functions.len() {
            for j in 0..i {
                if functions[i].name != functions[j].name {
                    continue;
                }
                let same_params =
                    signatures[i].params.iter().map(|(_, t)| t).eq(signatures[j].params.iter().map(|(_, t)| t));
                if same_params {
                    self.errors.push(AnalysisError::new(
                        functions[i].id,
                        functions[i].span,
                        AnalysisErrorKind::Redeclaration {
                            name: functions[i].name.clone(),
                            previous: Some(functions[j].span),
                        },
                    ));
                    break;
                }
            }
        }
    }

    /// A top-level struct's *signature* only: field types, plus every
    /// method's signature, with zero recursion into any method body. Unlike
    /// the pre-cross-module-cycle-fix version of this, `cell` is created (and
    /// registered in `omega_driver::Driver`'s global `struct_cells`, keyed by
    /// `(module_path, name)`) by the *caller* before this ever runs, not by
    /// this method itself -- so a self-referencing field (`next: *Node`) or a
    /// same- or cross-module mutual one resolves via `Context::resolve_type`'s
    /// resolver fallback finding this exact struct already `InProgress` in
    /// `Driver`'s global query state, not via anything local to this one
    /// throwaway `Analyzer`/`Context`. This method's only job is to populate
    /// `cell` in place, patched via `RefCell` so every earlier clone of it
    /// (e.g. one taken for a pointer field while this was still empty)
    /// observes the final result too.
    /// `method_ids` supplies, positionally (one per `s.functions`), the
    /// `HirId` each method's `ResolvedMethod.decl_id` gets stamped with --
    /// `f.id` itself for an ordinary (non-generic) struct, or a freshly
    /// minted synthetic id per generic instantiation (decided once by
    /// `omega_driver::Driver::compute_item`, the single source of truth for
    /// instantiation identity -- see its doc comment). `check_struct_body`
    /// reads these same ids back out of `cell` rather than ever recomputing
    /// them, so both phases agree on one identity per instantiation.
    pub fn signature_of_struct(
        &mut self,
        s: &HirStructDef,
        cell: &Rc<RefCell<ResolvedStructType>>,
        method_ids: &[HirId],
    ) -> (Option<()>, Vec<PendingSpecMethod>) {
        let Some(fields) = self.analyze_struct_fields(&s.fields) else { return (None, vec![]) };
        cell.borrow_mut().fields = fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect();

        self.context.enter_scope();
        let functions = self.analyze_all(&s.functions, Self::collect_function_signature);
        self.context.leave_scope();
        let Some(functions) = functions else { return (None, vec![]) };
        self.check_overload_duplicates(&s.functions, &functions);
        let own_functions: Vec<(Ident, ResolvedMethod)> = s
            .functions
            .iter()
            .zip(functions)
            .zip(method_ids)
            .map(|((f, fn_type), &decl_id)| (f.name.clone(), ResolvedMethod { decl_id, fn_type }))
            .collect();

        let self_type = ResolvedType::Struct(cell.clone());
        let (extra_methods, pending) =
            self.resolve_implements_clause(s.id, s.span, &s.name, &s.implements, &own_functions, &self_type);
        let mut all_functions = own_functions;
        all_functions.extend(extra_methods);
        cell.borrow_mut().functions = all_functions;

        (Some(()), pending)
    }

    /// A union's *signature* -- identical contract to `signature_of_struct`
    /// (see its doc comment); field overlap in storage is entirely a
    /// codegen-layout concern, invisible at this level.
    pub fn signature_of_union(
        &mut self,
        u: &HirUnionDef,
        cell: &Rc<RefCell<ResolvedUnionType>>,
        method_ids: &[HirId],
    ) -> (Option<()>, Vec<PendingSpecMethod>) {
        let Some(fields) = self.analyze_struct_fields(&u.fields) else { return (None, vec![]) };
        cell.borrow_mut().fields = fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect();

        self.context.enter_scope();
        let functions = self.analyze_all(&u.functions, Self::collect_function_signature);
        self.context.leave_scope();
        let Some(functions) = functions else { return (None, vec![]) };
        self.check_overload_duplicates(&u.functions, &functions);
        let own_functions: Vec<(Ident, ResolvedMethod)> = u
            .functions
            .iter()
            .zip(functions)
            .zip(method_ids)
            .map(|((f, fn_type), &decl_id)| (f.name.clone(), ResolvedMethod { decl_id, fn_type }))
            .collect();

        let self_type = ResolvedType::Union(cell.clone());
        let (extra_methods, pending) =
            self.resolve_implements_clause(u.id, u.span, &u.name, &u.implements, &own_functions, &self_type);
        let mut all_functions = own_functions;
        all_functions.extend(extra_methods);
        cell.borrow_mut().functions = all_functions;

        (Some(()), pending)
    }

    /// A top-level enum's *signature*: tag type, header fields, every
    /// variant (tag value, header constants, body fields), and every
    /// function's signature -- populated into `cell` in place, exactly like
    /// `signature_of_struct` (whose cell/`method_ids` contract this
    /// shares; see its doc comment). Keep-going discipline throughout: all
    /// independent problems in one enum definition are reported in one
    /// pass, and the cell is only ever populated when everything held.
    pub fn signature_of_enum(
        &mut self,
        e: &HirEnumDef,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        method_ids: &[HirId],
    ) -> (Option<()>, Vec<PendingSpecMethod>) {
        let mut ok = true;

        // --- header: optional leading explicit tag, then shared fields ---
        let mut explicit_tag: Option<ResolvedType> = None;
        let mut header: Vec<(Ident, ResolvedType)> = Vec::new();
        let mut seen_header: HashMap<Ident, Span> = HashMap::new();
        for (position, field) in e.header.iter().enumerate() {
            if field.ident.as_ref() == "tag" {
                if position != 0 {
                    self.errors
                        .push(AnalysisError::new(field.id, field.span, AnalysisErrorKind::EnumTagNotFirst));
                    ok = false;
                    continue;
                }
                let Some(tag_type) = self.resolve_type_or_error(field.id, field.span, &field.r#type, true) else {
                    ok = false;
                    continue;
                };
                let is_integer = matches!(
                    tag_type.numeric_kind(),
                    Some(NumericKind::Signed(_) | NumericKind::Unsigned(_))
                );
                if !is_integer {
                    self.errors.push(AnalysisError::new(
                        field.id,
                        field.span,
                        AnalysisErrorKind::EnumTagNotInteger { found: tag_type },
                    ));
                    ok = false;
                    continue;
                }
                explicit_tag = Some(tag_type);
                continue;
            }
            if seen_header.insert(field.ident.clone(), field.span).is_some() {
                self.errors.push(AnalysisError::new(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision { field: field.ident.clone(), variant: None },
                ));
                ok = false;
                continue;
            }
            // Header fields are laid out inline in every enum value -- the
            // same `indirect = false` a struct field passes.
            let Some(resolved) = self.resolve_type_or_error(field.id, field.span, &field.r#type, false) else {
                ok = false;
                continue;
            };
            if !Self::const_representable(&resolved) {
                self.errors.push(AnalysisError::new(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumHeaderFieldUnsupportedType {
                        field: field.ident.clone(),
                        found: resolved,
                    },
                ));
                ok = false;
                continue;
            }
            header.push((field.ident.clone(), resolved));
        }
        let has_tag = explicit_tag.is_some();
        let tag_type = explicit_tag.unwrap_or(ResolvedType::U16);

        // A broken header makes every variant's expectations (argument
        // count, tag-ness, field types) unknowable -- checking variants
        // against the *surviving* entries would only report derived noise
        // ("must supply 1 value" because the invalid tag entry was
        // dropped), so the header's own errors stand alone.
        if !ok {
            return (None, vec![]);
        }

        // --- shared dynamic fields: present on every variant like the
        // header, but runtime-valued -- no per-variant constant to parse,
        // so this is header validation's `const_representable`-free,
        // tag-free sibling. ---
        let mut dynamic_fields: Vec<(Ident, ResolvedType)> = Vec::new();
        let mut seen_dynamic: HashMap<Ident, Span> = HashMap::new();
        for field in &e.dynamic_fields {
            let collides = field.ident.as_ref() == "tag"
                || header.iter().any(|(name, _)| *name == field.ident)
                || seen_dynamic.contains_key(&field.ident);
            if collides {
                self.errors.push(AnalysisError::new(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision { field: field.ident.clone(), variant: None },
                ));
                ok = false;
                continue;
            }
            seen_dynamic.insert(field.ident.clone(), field.span);
            // Laid out inline in every enum value, exactly like a header
            // field -- the same `indirect = false` a struct field passes.
            let Some(resolved) = self.resolve_type_or_error(field.id, field.span, &field.r#type, false) else {
                ok = false;
                continue;
            };
            dynamic_fields.push((field.ident.clone(), resolved));
        }

        // Same reasoning as the header's own early bail just above: a
        // broken dynamic-fields list makes the variant loop's shadow check
        // against it unreliable.
        if !ok {
            return (None, vec![]);
        }

        // --- variants ---
        let mut variants: Vec<ResolvedEnumVariant> = Vec::new();
        let mut seen_variants: HashMap<Ident, Span> = HashMap::new();
        let mut seen_tags: HashMap<i128, (Ident, Span)> = HashMap::new();
        for (declared_index, variant) in e.variants.iter().enumerate() {
            if let Some(previous) = seen_variants.insert(variant.name.clone(), variant.span) {
                self.errors.push(AnalysisError::new(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::Redeclaration { name: variant.name.clone(), previous: Some(previous) },
                ));
                ok = false;
                continue;
            }

            let expected_args = header.len() + has_tag as usize;
            if variant.args.len() != expected_args {
                self.errors.push(AnalysisError::new(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::EnumVariantArgCount {
                        variant: variant.name.clone(),
                        expected: expected_args,
                        found: variant.args.len(),
                        has_tag,
                    },
                ));
                ok = false;
                continue;
            }

            // The tag: the leading argument when explicit, or this
            // variant's declared position when implicit (`u16`, counting
            // from 0 -- guaranteed in range: u16::MAX variants is far past
            // any real declaration, and `declared_index` is bounded by the
            // source's own variant count).
            let tag = if has_tag {
                match self.const_eval(&variant.args[0], &tag_type) {
                    Some(ConstValue::Number(value)) => Some(value),
                    Some(_) => unreachable!("const_eval only produces Number for an integer expected type"),
                    None => None,
                }
            } else {
                Some(NumberValue::Unsigned(declared_index as u64))
            };
            let Some(tag) = tag else {
                ok = false;
                continue;
            };
            let tag_key = match tag {
                NumberValue::Signed(value) => value as i128,
                NumberValue::Unsigned(value) => value as i128,
                NumberValue::Float(_) => unreachable!("tag types are integers"),
            };
            if let Some((previous_variant, previous)) = seen_tags.get(&tag_key) {
                self.errors.push(AnalysisError::new(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::DuplicateEnumTag {
                        variant: variant.name.clone(),
                        value: tag_key.to_string(),
                        previous_variant: previous_variant.clone(),
                        previous: *previous,
                    },
                ));
                ok = false;
                continue;
            }
            seen_tags.insert(tag_key, (variant.name.clone(), variant.span));

            // Header values -- one constant per header field, positionally.
            let mut header_values = Vec::with_capacity(header.len());
            let mut variant_ok = true;
            for ((_, field_type), arg) in header.iter().zip(&variant.args[has_tag as usize..]) {
                match self.const_eval(arg, field_type) {
                    Some(value) => header_values.push(value),
                    None => variant_ok = false,
                }
            }

            // Body fields -- must not collide with the header, the shared
            // dynamic fields (all three are reached as `value.name`), or
            // the reserved `tag`.
            let mut fields: Vec<(Ident, ResolvedType)> = Vec::new();
            let mut seen_fields: HashMap<Ident, Span> = HashMap::new();
            for field in &variant.fields {
                let shadows_header = field.ident.as_ref() == "tag"
                    || header.iter().any(|(name, _)| *name == field.ident)
                    || dynamic_fields.iter().any(|(name, _)| *name == field.ident);
                if shadows_header {
                    self.errors.push(AnalysisError::new(
                        field.id,
                        field.span,
                        AnalysisErrorKind::EnumFieldNameCollision {
                            field: field.ident.clone(),
                            variant: Some(variant.name.clone()),
                        },
                    ));
                    variant_ok = false;
                    continue;
                }
                if let Some(previous) = seen_fields.insert(field.ident.clone(), field.span) {
                    self.errors.push(AnalysisError::new(
                        field.id,
                        field.span,
                        AnalysisErrorKind::Redeclaration { name: field.ident.clone(), previous: Some(previous) },
                    ));
                    variant_ok = false;
                    continue;
                }
                // A body field is inline layout, exactly like a struct
                // field -- the one context that catches by-value recursion.
                let Some(resolved) = self.resolve_type_or_error(field.id, field.span, &field.r#type, false) else {
                    variant_ok = false;
                    continue;
                };
                fields.push((field.ident.clone(), resolved));
            }

            if !variant_ok {
                ok = false;
                continue;
            }
            variants.push(ResolvedEnumVariant { name: variant.name.clone(), tag, header_values, fields });
        }

        // A function sharing a variant's name would make `Enum::name`
        // ambiguous -- rejected outright, before signatures are collected.
        for function in &e.functions {
            if let Some(previous) = seen_variants.get(&function.name) {
                self.errors.push(AnalysisError::new(
                    function.id,
                    function.span,
                    AnalysisErrorKind::Redeclaration { name: function.name.clone(), previous: Some(*previous) },
                ));
                ok = false;
            }
        }

        if !ok {
            return (None, vec![]);
        }

        {
            let mut resolved = cell.borrow_mut();
            resolved.tag_type = tag_type;
            resolved.header = header;
            resolved.dynamic_fields = dynamic_fields;
            resolved.variants = variants;
        }

        self.context.enter_scope();
        let functions = self.analyze_all(&e.functions, Self::collect_function_signature);
        self.context.leave_scope();
        let Some(functions) = functions else { return (None, vec![]) };
        self.check_overload_duplicates(&e.functions, &functions);
        let own_functions: Vec<(Ident, ResolvedMethod)> = e
            .functions
            .iter()
            .zip(functions)
            .zip(method_ids)
            .map(|((f, fn_type), &decl_id)| (f.name.clone(), ResolvedMethod { decl_id, fn_type }))
            .collect();

        let self_type = ResolvedType::Enum { cell: cell.clone(), variant: None };
        let (extra_methods, pending) =
            self.resolve_implements_clause(e.id, e.span, &e.name, &e.implements, &own_functions, &self_type);
        let mut all_functions = own_functions;
        all_functions.extend(extra_methods);
        cell.borrow_mut().functions = all_functions;

        (Some(()), pending)
    }

    /// Whether `r#type` has a literal constant form -- the requirement on
    /// enum header fields (their values are per-variant constants); see
    /// `ConstValue`.
    fn const_representable(r#type: &ResolvedType) -> bool {
        r#type.numeric_kind().is_some()
            || matches!(r#type, ResolvedType::Bool | ResolvedType::Char)
            // A string constant's own type is always immutable (see
            // `HirExpr::String`'s arm in `analyze_expr`), so only an
            // immutable `*u8` header field could ever accept one anyway.
            || matches!(r#type, ResolvedType::Pointer { pointee, mutable: false } if **pointee == ResolvedType::U8)
            // A compile-time slice (`&[...]`) is likewise always immutable
            // -- see `ConstValue::Slice`.
            || matches!(r#type, ResolvedType::Slice { item, mutable: false } if Self::const_representable(item))
            // A fixed-length array's own length is part of the field's
            // type, shared by every variant -- see `ConstValue::Array`.
            || matches!(r#type, ResolvedType::SizedArray(item, _) if Self::const_representable(item))
    }

    /// Evaluates an enum variant's tag/header value: a literal (number,
    /// string, bool, or char -- optionally a negated number), checked
    /// against `expected` -- the *expected type drives* number-literal
    /// interpretation here (no `u32` suffix needed to satisfy a `u32`
    /// header field), unlike ordinary expressions, since a constant
    /// position has nothing else to infer from.
    fn const_eval(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        let mismatch = |this: &mut Self, found: &str| {
            this.errors.push(AnalysisError::new(
                expr.id,
                expr.span,
                AnalysisErrorKind::EnumValueTypeMismatch { expected: expected.clone(), found: found.into() },
            ));
            None
        };
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => {
                    self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number)
                }
                _ => {
                    self.errors
                        .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::EnumValueNotConstant));
                    None
                }
            },
            HirExpr::String(s) => match expected {
                ResolvedType::Pointer { pointee, mutable: false } if **pointee == ResolvedType::U8 => {
                    Some(ConstValue::Str(s.0.clone()))
                }
                _ => mismatch(self, "a string literal"),
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => mismatch(self, "a bool literal"),
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => mismatch(self, "a character literal"),
            },
            // A bare `[...]` -- unlike a compile-time *slice* (`&[...]`,
            // just below), a fixed-length array has no pointer indirection
            // at all: its elements live inline, directly in the header's
            // own storage, so there's nothing to take the address of --
            // matching how an ordinary `[T; N]`-typed place is never
            // written with a leading `&` either. Every variant must supply
            // exactly `size` elements (the length is part of the field's
            // declared type, shared by every variant, unlike a slice's
            // per-variant length).
            HirExpr::ArrayLiteral(elements) => match expected {
                ResolvedType::SizedArray(item, size) => {
                    if elements.len() != *size as usize {
                        return mismatch(self, &format!("an array literal with {} elements", elements.len()));
                    }
                    let mut values = Vec::with_capacity(elements.len());
                    for element in elements {
                        values.push(self.const_eval(element, item)?);
                    }
                    Some(ConstValue::Array(values))
                }
                _ => mismatch(self, "an array literal"),
            },
            // `&[...]` is the *only* recognized spelling for a compile-time
            // slice, even here -- a bare `[...]` is never treated as one
            // (it would be too easy to confuse with an ordinary array),
            // matching `analyze_const_slice`'s own requirement in ordinary
            // expression position. Recurses through `const_eval` itself, so
            // nesting (e.g. a `*[*[i32]]` header field, written
            // `&[&[1, 2], &[3, 4]]`) falls out for free.
            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                if *mutable {
                    self.errors
                        .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::ConstSliceCannotBeMutable));
                    return None;
                }
                match &base.expr {
                    HirExpr::ArrayLiteral(elements) => match expected {
                        ResolvedType::Slice { item, mutable: false } => {
                            let mut values = Vec::with_capacity(elements.len());
                            for element in elements {
                                values.push(self.const_eval(element, item)?);
                            }
                            Some(ConstValue::Slice(values))
                        }
                        _ => mismatch(self, "an array literal"),
                    },
                    _ => {
                        self.errors
                            .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::EnumValueNotConstant));
                        None
                    }
                }
            }
            _ => {
                self.errors
                    .push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::EnumValueNotConstant));
                None
            }
        }
    }

    /// The number-literal side of `const_eval`: parses and range-checks `n`
    /// against `expected` (which must be numeric), honoring an optional
    /// leading negation -- including the asymmetric edge (`-32768` fits
    /// `i16`, `32768` doesn't).
    fn const_number(
        &mut self,
        node_id: HirId,
        span: Span,
        n: &NumberExpr,
        expected: &ResolvedType,
        negated: bool,
    ) -> Option<NumberValue> {
        let mismatch = |this: &mut Self, found: String| {
            this.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::EnumValueTypeMismatch { expected: expected.clone(), found },
            ));
            None
        };
        let Some(kind) = expected.numeric_kind() else {
            return mismatch(self, "a number literal".into());
        };

        // An explicit suffix must agree with the field's declared type --
        // there are no implicit conversions to paper over a disagreement.
        if let Some(suffix) = &n.explicit_type {
            let suffixed = self.context.resolve_type(
                Type::Named(suffix.clone().into()),
                &mut *self.resolver,
                &self.module_path,
                true,
            );
            match suffixed {
                Ok(t) if t == *expected => {}
                Ok(t) => return mismatch(self, format!("a `{t}` literal")),
                Err(_) => {
                    self.errors
                        .push(AnalysisError::new(node_id, span, AnalysisErrorKind::InvalidNumberType(suffix.clone())));
                    return None;
                }
            }
        }

        let is_float = matches!(kind, NumericKind::Float(_));
        if n.fractional_part.is_some() && !is_float {
            return mismatch(self, "a fractional number literal".into());
        }
        if negated && matches!(kind, NumericKind::Unsigned(_)) {
            return mismatch(self, "a negative number literal".into());
        }

        let literal_text = || {
            let digits = match &n.fractional_part {
                Some(frac) => format!("{}.{}", n.integer_part, frac),
                None => n.integer_part.clone(),
            };
            if negated { format!("-{digits}") } else { digits }
        };
        let out_of_range = |this: &mut Self| {
            this.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::NumberLiteralOutOfRange { literal: literal_text(), r#type: expected.clone() },
            ));
            None
        };

        match kind {
            NumericKind::Float(width) => {
                let text = format!("{}.{}", n.integer_part, n.fractional_part.as_deref().unwrap_or("0"));
                let Ok(parsed) = text.parse::<f64>() else {
                    return out_of_range(self);
                };
                if width == 32 && parsed.is_finite() && (parsed as f32).is_infinite() {
                    return out_of_range(self);
                }
                Some(NumberValue::Float(if negated { -parsed } else { parsed }))
            }
            NumericKind::Signed(width) => {
                let Ok(parsed) = u64::from_str_radix(&n.integer_part, n.base.radix()) else {
                    return out_of_range(self);
                };
                // One extra magnitude on the negative side: |i16::MIN| is
                // 32768, one past i16::MAX.
                let positive_max = if width == 64 { i64::MAX as u64 } else { (1u64 << (width - 1)) - 1 };
                let max = if negated { positive_max + 1 } else { positive_max };
                if parsed > max {
                    return out_of_range(self);
                }
                let value = if negated { (-(parsed as i128)) as i64 } else { parsed as i64 };
                Some(NumberValue::Signed(value))
            }
            NumericKind::Unsigned(width) => {
                let Ok(parsed) = u64::from_str_radix(&n.integer_part, n.base.radix()) else {
                    return out_of_range(self);
                };
                let max = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
                if parsed > max {
                    return out_of_range(self);
                }
                Some(NumberValue::Unsigned(parsed))
            }
        }
    }

    /// Checks a top-level enum's function *bodies* only -- the counterpart
    /// of `check_struct_body`, with the same read-back-from-the-cell
    /// discipline (see its doc comment); an enum's fields/variants have no
    /// body work of their own (their values were fully evaluated during
    /// `signature_of_enum`).
    pub fn check_enum_body(
        &mut self,
        e: &HirEnumDef,
        cell: &Rc<RefCell<ResolvedEnumType>>,
    ) -> Option<CheckedEnumDef> {
        let methods: Vec<(ResolvedFunctionType, HirId)> =
            cell.borrow().functions.iter().map(|(_, method)| (method.fn_type.clone(), method.decl_id)).collect();

        self.context.enter_scope();
        let mut functions = Vec::with_capacity(e.functions.len());
        let mut ok = true;
        for (f, (fn_type, decl_id)) in e.functions.iter().zip(methods.iter()) {
            match self.check_function_body(f, fn_type, *decl_id) {
                Some(checked) => functions.push(checked),
                None => ok = false,
            }
        }
        self.context.leave_scope();
        if !ok {
            return None;
        }

        Some(CheckedEnumDef { id: e.id, span: e.span, name: e.name.clone(), functions })
    }

    /// Checks a function's (or method's) *body* only -- its signature, and
    /// its own name bound so any call to it (including a recursive one from
    /// its own body) resolves, are already handled by
    /// `omega_driver::Driver::ensure_item`/`collect_function_signature`.
    /// Enters a fresh scope to bind `f`'s params by name (signature
    /// collection only ever resolved their *types*, never bound them --
    /// that's a body-analysis-time concern, same as it always was).
    /// `id` is stamped onto the produced `CheckedFunctionDef` in place of
    /// always reading `f.id` -- for an ordinary (non-generic) function this
    /// is just `f.id` (behavior-preserving); for a generic instantiation
    /// it's the same freshly-minted synthetic id `omega_driver::Driver`
    /// already decided (and stored) during the signature phase, so codegen
    /// gets one distinct compiled function per instantiation.
    pub fn check_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
    ) -> Option<CheckedFunctionDef> {
        self.context.enter_scope();
        let params = self.analyze_all(&f.params, Self::analyze_param);

        // One `Analyzer` checks exactly one top-level item at a time (see
        // `item_name`'s doc comment), and a struct's methods are checked
        // sequentially, never while another method/function's body is still
        // being analyzed -- so there's no *nesting* to protect against here,
        // just an ordinary reset before each independent body: no enclosing
        // loop or defer of its own, and its own declared return type.
        self.current_return_type = (*fn_type.return_type).clone();
        self.loop_stack.clear();
        self.in_defer_body = false;
        // The function's own declared return type is the expected type for
        // an implicit tail-expression return (`fn f() => f64 { 10 }`) --
        // the same untyped-constant adaptation an explicit `return 10;`
        // gets (see `HirStmt::Return`'s arm above).
        let body = self.analyze_block(&f.body, Some(fn_type.return_type.as_ref()));

        self.context.leave_scope();

        let params = params?;
        let body = body?;
        self.check_function_return(f.id, f.span, &fn_type.return_type, &body)?;

        Some(CheckedFunctionDef {
            id,
            span: f.span,
            name: f.name.clone(),
            is_member_function: f.is_member_function,
            is_variadic: false,
            params,
            return_type: (*fn_type.return_type).clone(),
            body,
        })
    }

    /// Checks one queued default-method instantiation's body (see
    /// `PendingSpecMethod`) -- reconstructs an ordinary, synthetic
    /// `HirFunctionDef` straight out of the spec's own raw signature (see
    /// `RawSpecFunctionSig`'s doc comment for why it carries real
    /// `HirParam`s, not just names/types) and reuses `check_function_body`
    /// wholesale, rather than duplicating its param-binding/return-checking
    /// logic. The caller (`omega_driver::Driver::check_item_body`)
    /// constructs `self` fresh, seeded with exactly `pending.substitution`
    /// (`Self` + the owning spec's own generics) -- the implementor's own
    /// generics are never relevant here, since the spec's HIR can't
    /// reference a name it doesn't know about.
    pub fn check_pending_spec_method(&mut self, pending: &PendingSpecMethod) -> Option<CheckedFunctionDef> {
        let body = pending
            .raw
            .default_body
            .clone()
            .expect("only ever queued (resolve_implements_clause) when a default body exists");
        let synthetic = HirFunctionDef {
            id: pending.raw.decl_id,
            span: pending.raw.span,
            name: pending.raw.name.clone(),
            generics: vec![],
            is_member_function: pending.raw.is_member_function,
            params: pending.raw.params.clone(),
            return_type: pending.raw.return_type.clone(),
            body,
        };
        self.check_function_body(&synthetic, &pending.fn_type, pending.id)
    }

    /// Checks a top-level struct's methods' *bodies* only -- its fields and
    /// every method's signature are already fully resolved (see
    /// `signature_of_struct`), sitting in `cell` (the same shared cell
    /// `omega_driver::Driver` created before that ran); `fields`/`functions`
    /// here are read back out of it (zipped positionally against
    /// `s.fields`/`s.functions`, in the same order `signature_of_struct`
    /// built them in) rather than recomputed, so a self-referencing field
    /// reads back the exact same live data `analyze_place`'s field
    /// projections will later see.
    pub fn check_struct_body(&mut self, s: &HirStructDef, cell: &Rc<RefCell<ResolvedStructType>>) -> Option<CheckedStructDef> {
        let fields = s
            .fields
            .iter()
            .zip(cell.borrow().fields.iter())
            .map(|(hir_field, (_, r#type))| CheckedParam {
                id: hir_field.id,
                span: hir_field.span,
                ident: hir_field.ident.clone(),
                r#type: r#type.clone(),
            })
            .collect();

        // `decl_id` is read back from the cell (not `f.id`) so a generic
        // instantiation's methods get the same synthetic ids the signature
        // phase already decided -- see `signature_of_struct`'s doc comment.
        let methods: Vec<(ResolvedFunctionType, HirId)> =
            cell.borrow().functions.iter().map(|(_, method)| (method.fn_type.clone(), method.decl_id)).collect();

        self.context.enter_scope();
        let mut functions = Vec::with_capacity(s.functions.len());
        let mut ok = true;
        for (f, (fn_type, decl_id)) in s.functions.iter().zip(methods.iter()) {
            match self.check_function_body(f, fn_type, *decl_id) {
                Some(checked) => functions.push(checked),
                None => ok = false,
            }
        }
        self.context.leave_scope();
        if !ok {
            return None;
        }

        Some(CheckedStructDef { id: s.id, span: s.span, name: s.name.clone(), fields, functions })
    }

    /// Checks a union's methods' *bodies* only -- identical contract to
    /// `check_struct_body` (see its doc comment).
    pub fn check_union_body(&mut self, u: &HirUnionDef, cell: &Rc<RefCell<ResolvedUnionType>>) -> Option<CheckedUnionDef> {
        let fields = u
            .fields
            .iter()
            .zip(cell.borrow().fields.iter())
            .map(|(hir_field, (_, r#type))| CheckedParam {
                id: hir_field.id,
                span: hir_field.span,
                ident: hir_field.ident.clone(),
                r#type: r#type.clone(),
            })
            .collect();

        let methods: Vec<(ResolvedFunctionType, HirId)> =
            cell.borrow().functions.iter().map(|(_, method)| (method.fn_type.clone(), method.decl_id)).collect();

        self.context.enter_scope();
        let mut functions = Vec::with_capacity(u.functions.len());
        let mut ok = true;
        for (f, (fn_type, decl_id)) in u.functions.iter().zip(methods.iter()) {
            match self.check_function_body(f, fn_type, *decl_id) {
                Some(checked) => functions.push(checked),
                None => ok = false,
            }
        }
        self.context.leave_scope();
        if !ok {
            return None;
        }

        Some(CheckedUnionDef { id: u.id, span: u.span, name: u.name.clone(), fields, functions })
    }
}

