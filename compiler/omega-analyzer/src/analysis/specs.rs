use super::*;

/// One spec function requirement, flattened out of a (possibly generic,
/// possibly multiply-inherited) spec reference and resolved for one
/// specific concrete implementor -- see `Analyzer::flatten_spec`.
pub(super) struct FlattenedSpecFn {
    pub(super) name: Ident,
    /// For a `spec T` (static-dispatch, associated-type-like) return
    /// requirement, `fn_type.return_type` is an inert `ResolvedType::Void`
    /// placeholder -- **never read** in that case; `return_type_bound`
    /// (below) is the source of truth instead. See its own doc comment.
    pub(super) fn_type: ResolvedFunctionType,
    /// `Some((spec, type_args))` when this requirement's return type was
    /// declared `=> spec Bound<...>` rather than an ordinary concrete type
    /// -- an implementor satisfies it with *any* concrete return type that
    /// itself implements `Bound<...>` (checked via `type_implements_spec`),
    /// not by exact-`ResolvedType`-equality the way every other requirement
    /// still is. `None` for the overwhelmingly common case, preserving
    /// today's exact-equality behavior unchanged. See
    /// `fn_satisfies_requirement`/`requirements_are_same`, the two places
    /// this is actually consulted.
    pub(super) return_type_bound: Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    pub(super) raw: RawSpecFunctionSig,
    /// The spec that *declares* this function, by `HirId` -- two different
    /// specs may declare the same name and signature, and those are
    /// **different functions**. `(spec_id, type_args, name)` is the
    /// requirement's full identity, keyed on by both conformance checking
    /// (`type_implements_spec`'s alias branch) and vtable slot construction
    /// -- a bare name no longer identifies a method, now that one spec can
    /// flatten two colliding declarations side by side.
    pub(super) spec_id: HirId,
    /// The declaring spec's name, kept for diagnostics (`MissingSpecFunction`
    /// names it) -- never for identity.
    pub(super) spec_name: Ident,
    /// The visibility of whichever spec directly declares this function
    /// (`spec_name`'s own `ResolvedSpecType::visibility`) -- a spec
    /// function has no per-function modifier of its own (see
    /// `SpecFunctionStmt`), it always inherits its declaring spec's.
    /// Tracked per function because an alias member can have a different
    /// visibility than the alias itself -- each function keeps the
    /// visibility of the spec that actually declared it, exactly the same
    /// way `spec_name` already does. This is inherited by the conforming
    /// method.
    pub(super) visibility: Visibility,
    /// `Self` + the owning spec's own generics, bound to concrete types --
    /// exactly what resolved `fn_type` above, kept around so a queued
    /// default instantiation's *body* can be checked later (phase 2, see
    /// `PendingSpecMethod`) with the identical substitution its signature
    /// already used.
    pub(super) substitution: Vec<(Ident, ResolvedType)>,
}

impl FlattenedSpecFn {
    /// The declaring spec's own concrete type arguments -- `substitution`
    /// always starts with `("Self", self_type)` followed by the declaring
    /// spec's generics in declaration order (see where `substitution` is
    /// built in `Analyzer::flatten_spec_into`), so this is just that
    /// leading `Self` entry dropped. Part of the requirement's identity
    /// (`spec_id`, these args, `name`) -- the same spec implemented at two
    /// different type arguments is two different requirements, and a
    /// diagnostic naming `spec_name` (e.g. `MissingSpecFunction`) can also
    /// show *which* instantiation it's about -- `"Consumer<*u8>"`, not just
    /// `"Consumer"`.
    pub(super) fn type_args(&self) -> Vec<ResolvedType> {
        self.substitution[1..]
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect()
    }
}

/// A spec-default method an implementor needs (no override, spec supplied
/// a body) -- signature already resolved and merged into the implementor's
/// conforming method list in phase 1; this is what phase 2 still needs to
/// check with the same `Self`/generics binding --
/// see `Analyzer::check_pending_spec_method`.
#[derive(Clone)]
pub struct PendingSpecMethod {
    pub id: HirId,
    pub fn_type: ResolvedFunctionType,
    /// See `FlattenedSpecFn::return_type_bound`'s doc comment -- propagated
    /// unchanged from the `FlattenedSpecFn` this was queued from. A default
    /// body queued with this `Some` still needs its concrete return type
    /// inferred from its own body (the same machinery an ordinary `spec T`-
    /// returning function uses) before `check_pending_spec_method` can
    /// check it for real -- not yet wired in (`fn_type.return_type` is
    /// still the inert placeholder in that case today).
    pub return_type_bound: Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    pub raw: RawSpecFunctionSig,
    pub substitution: Vec<(Ident, ResolvedType)>,
}

impl<'r> Analyzer<'r> {
    /// A `primitive Target { ... }` block's own target.
    ///
    /// Deliberately *not* `resolve_conform_target`, which this used to reuse.
    /// The two answer different questions: a conform target is a type that can
    /// own an implementation of a spec, while a primitive target is a built-in
    /// type's **declaration site** in `core`. Those sets genuinely differ at
    /// both ends -- a struct is conformable but is not a primitive, and
    /// `void`/`never` are primitives that nothing could ever conform (there is
    /// no value to implement a spec method on).
    ///
    /// Borrowing the conformance gate meant the stricter rule always won, so
    /// `ConformanceRegistry::primitive_target_allowed` could never widen past
    /// it -- which is why `void` was rejected with "conform target is not a
    /// concrete type", a diagnostic about a construct the author never wrote.
    /// `allow_never` is set for the same reason: `never` is barred from
    /// ordinary type positions, but its declaration site is not one.
    pub fn resolve_primitive_target(
        &mut self,
        id: HirId,
        span: Span,
        target: &Type,
    ) -> Option<ResolvedType> {
        if let Type::Named(path) = target
            && path.is_unqualified()
            && path.head.as_ref() == "str"
        {
            return Some(ResolvedType::Str { mutable: false });
        }
        if let Type::InferredArray(item) = target {
            let item = self.resolve_type_or_error(id, span, item, true)?;
            return Some(ResolvedType::Slice {
                item: Box::new(item),
                mutable: false,
            });
        }
        // Whether this particular built-in may carry a block is
        // `primitive_target_allowed`'s question, asked by the caller against
        // the resolved type; all this has to do is resolve it.
        self.resolve_type_or_error_checked(id, span, target, true, true)
    }

    pub fn resolve_conform_target(
        &mut self,
        id: HirId,
        span: Span,
        target: &Type,
    ) -> Option<ResolvedType> {
        if let Type::Named(path) = target
            && path.is_unqualified()
            && path.head.as_ref() == "str"
        {
            return Some(ResolvedType::Str { mutable: false });
        }
        if let Type::InferredArray(item) = target {
            let item = self.resolve_type_or_error(id, span, item, true)?;
            return Some(ResolvedType::Slice {
                item: Box::new(item),
                mutable: false,
            });
        }
        // A conformance has a concrete owner.  The parser intentionally
        // accepts every type-shaped token sequence here so this semantic
        // gate, rather than parser recovery, owns the diagnostic for shapes
        // which can never own a conformance.
        if matches!(
            target,
            Type::Pointer(..)
                | Type::UnknownSizeArray(..)
                | Type::SizedArray(..)
                | Type::Function(..)
                | Type::SpecObject(..)
                | Type::SpecStatic(..)
        ) {
            self.error(id, span, AnalysisErrorKind::ConformTargetNotAType);
            return None;
        }
        let resolved = self.resolve_type_or_error(id, span, target, true)?;
        if !Self::is_conformable_target(&resolved) {
            self.error(id, span, AnalysisErrorKind::ConformTargetNotAType);
            return None;
        }
        Some(resolved)
    }

    /// Whether a resolved type can own a `conform` block. This is shared by
    /// ordinary target resolution and blanket-template matching so the two
    /// paths cannot drift into accepting different target families.
    pub fn is_conformable_target(target: &ResolvedType) -> bool {
        matches!(
            target,
            ResolvedType::Bool
                | ResolvedType::Char
                | ResolvedType::I8
                | ResolvedType::I16
                | ResolvedType::I32
                | ResolvedType::I64
                | ResolvedType::ISize
                | ResolvedType::U8
                | ResolvedType::U16
                | ResolvedType::U32
                | ResolvedType::U64
                | ResolvedType::USize
                | ResolvedType::F32
                | ResolvedType::F64
                | ResolvedType::Slice { .. }
                | ResolvedType::Str { .. }
                | ResolvedType::Struct(..)
                | ResolvedType::Union(..)
                | ResolvedType::Enum { .. }
        )
    }

    pub fn check_conform_block(
        &mut self,
        id: HirId,
        span: Span,
        target: &ResolvedType,
        spec: &(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>),
        functions: &[HirFunctionDef],
        method_ids: &[HirId],
    ) -> Option<(
        Rc<RefCell<ResolvedSpecType>>,
        Vec<ResolvedType>,
        Vec<(Ident, ResolvedMethod)>,
        Vec<PendingSpecMethod>,
    )> {
        let (spec, spec_args) = spec.clone();
        let requirements = self.flatten_spec(id, span, &spec, &spec_args, target)?;
        self.context.enter_scope();
        let signatures = self.analyze_all(functions, |this, function| {
            this.collect_function_signature(function)
        });
        self.context.leave_scope();
        let signatures = signatures?;
        self.check_overload_duplicates(functions, &signatures);

        let source = ConformanceSource {
            spec: spec.clone(),
            spec_args: spec_args.clone(),
        };
        let mut methods = Vec::with_capacity(requirements.len());
        let mut pending = Vec::new();
        for requirement in requirements {
            let matching = functions
                .iter()
                .zip(&signatures)
                .zip(method_ids)
                .find(|((function, (signature, _)), _)| {
                    function.name == requirement.name
                        && self.fn_satisfies_requirement(
                            id,
                            span,
                            signature,
                            &requirement.fn_type,
                            &requirement.return_type_bound,
                        )
                });
            if let Some(((_function, (signature, annotations)), method_id)) = matching {
                methods.push((
                    requirement.name.clone(),
                    ResolvedMethod {
                        decl_id: *method_id,
                        fn_type: signature.clone(),
                        visibility: requirement.visibility,
                        annotations: annotations.clone(),
                        source: Some(source.clone()),
                    },
                ));
            } else if requirement.raw.default_body.is_some() {
                let minted_id = self.resolver.fresh_synthetic_id();
                methods.push((
                    requirement.name.clone(),
                    ResolvedMethod {
                        decl_id: minted_id,
                        fn_type: requirement.fn_type.clone(),
                        visibility: requirement.visibility,
                        annotations: crate::annotations::ResolvedAnnotations::default(),
                        source: Some(source.clone()),
                    },
                ));
                pending.push(PendingSpecMethod {
                    id: minted_id,
                    fn_type: requirement.fn_type,
                    return_type_bound: requirement.return_type_bound,
                    raw: requirement.raw,
                    substitution: requirement.substitution,
                });
            } else {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::MissingSpecFunction {
                        implementor: Ident(target.to_string()),
                        spec: requirement.spec_name.clone(),
                        spec_type_args: requirement.type_args(),
                        function: requirement.name,
                    },
                );
            }
        }
        let spec_name = spec.borrow().name.clone();
        for (function, method_id) in functions.iter().zip(method_ids) {
            if !methods
                .iter()
                .any(|(_, method)| method.decl_id == *method_id)
            {
                self.error(
                    function.id,
                    function.span,
                    AnalysisErrorKind::ConformanceExtraFunction {
                        spec: spec_name.clone(),
                        function: function.name.clone(),
                    },
                );
            }
        }
        Some((spec, spec_args, methods, pending))
    }
    /// Resolves a raw `Type` that's expected to name a spec (a conform target,
    /// spec dependency, or generic bound) to its cell plus
    /// its own resolved generic arguments (e.g. `Iterator<i32>`'s `[i32]`)
    /// -- `None` on failure (already reported, either as an ordinary
    /// `UnresolvedType` or, if it resolved to something other than a spec,
    /// `TypeResolutionError::NotASpec`).
    pub fn resolve_spec_reference(
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
        // `resolve_type_or_error_raw`, not `resolve_type_or_error`: a bare
        // spec name is exactly the expected result here (unlike everywhere
        // else that resolves a type), so this deliberately bypasses the
        // wrapper's bare-spec-is-never-a-value-type check.
        let resolved = self.resolve_type_or_error_raw(id, span, ty, true)?;
        if !ok {
            return None;
        }
        match resolved {
            ResolvedType::Spec(spec) => Some((spec, resolved_args)),
            _ => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name)),
                );
                None
            }
        }
    }

    /// Builds a spec's own raw (unresolved) function signature list --
    /// `RawSpecFunctionSig`'s doc comment explains why no type resolution
    /// happens here at all (deferred to `flatten_spec`, once a concrete
    /// implementor's `Self` is known). Checks only for a duplicate name
    /// among the spec's own functions -- a genuine signature conflict
    /// between an alias's *members* is a `flatten_spec`-time concern
    /// instead (only detectable once both sides are resolved with a concrete
    /// `Self`).
    /// Also resolves `sp.annotations` -- folded in here, not a separate
    /// function, because this is the one
    /// place a spec's declaration resolution (`omega_driver::Driver::
    /// resolve_spec_declaration`) converges on exactly once -- see that
    /// caller.
    pub fn resolve_spec_functions(
        &mut self,
        sp: &HirSpecDef,
    ) -> (
        Vec<(Ident, RawSpecFunctionSig)>,
        crate::annotations::ResolvedAnnotations,
    ) {
        let annotations = crate::annotations::resolve(
            self,
            sp.id,
            &sp.annotations,
            crate::annotations::ItemKind::Spec,
            false,
            false,
        );

        let mut functions = Vec::new();
        let mut seen: HashSet<Ident> = HashSet::new();
        for f in &sp.functions {
            if !seen.insert(f.name.clone()) {
                self.error(
                    f.id,
                    f.span,
                    AnalysisErrorKind::Redeclaration {
                        name: f.name.clone(),
                        previous: None,
                    },
                );
                continue;
            }
            if f.is_variadic {
                // Rejected at the spec's own declaration for the same reason
                // by-value `self` is, just below: nothing downstream could
                // ever satisfy it. Omega has no variadic function
                // *definitions* at all -- only `extern` declarations may be
                // variadic, for C interop -- so neither a `conform` block nor
                // a spec default can supply a matching body, and every
                // implementor would get a bare `MissingSpecFunction` naming a
                // function it has no syntax to write.
                //
                // The `is_variadic` plumbing behind this (HIR,
                // `RawSpecFunctionSig`, the resolved `ResolvedFunctionType`)
                // is complete and correct; only this guard stands between it
                // and working. Delete it the day variadic definitions exist.
                self.error(
                    f.id,
                    f.span,
                    AnalysisErrorKind::VariadicSpecFunctionUnsatisfiable {
                        name: f.name.clone(),
                    },
                );
            }
            let by_value = matches!(
                f.self_mode,
                Some(SelfMode::Value) | Some(SelfMode::MutValue)
            );
            if by_value {
                // `spec *T` dynamic dispatch erases `Self` down to a bare
                // data pointer (see `finish_dynamic_dispatch_call`) -- a
                // by-value self has no way to survive that, so it's
                // rejected here, unconditionally, rather than only where a
                // `spec *T` coercion actually happens (a spec used only for
                // static bounds today could gain one anywhere else in the
                // program later).
                self.error(
                    f.id,
                    f.span,
                    AnalysisErrorKind::SpecSelfMustBePointer {
                        name: f.name.clone(),
                    },
                );
            }
            functions.push((
                f.name.clone(),
                RawSpecFunctionSig {
                    decl_id: f.id,
                    name: f.name.clone(),
                    span: f.span,
                    self_mode: f.self_mode,
                    is_variadic: f.is_variadic,
                    params: f.params.clone(),
                    return_type: f.return_type.clone(),
                    default_body: f.body.clone(),
                },
            ));
        }
        (functions, annotations)
    }

    /// Resolves a spec's own alias member list (`spec AB = A + B`) to their
    /// cells, keeping each member's own type arguments **raw** (unresolved
    /// `Type`, not `ResolvedType`). Unlike `resolve_spec_reference` (used
    /// wherever a concrete reference's args are already resolvable -- a
    /// generic bound or conform declaration), this runs at the alias's own
    /// declaration, before its own generics are ever bound to anything
    /// concrete -- resolving a member's args here would fail for exactly the
    /// case that matters (`spec Foo<T> = Bar<T> + Baz;`), the same way
    /// `resolve_spec_functions` already stays raw for the identical reason.
    /// Only *which* spec each member names is resolved eagerly here, via
    /// `ModuleResolver::spec_declaration` (an args-independent lookup); the
    /// args themselves are resolved later, in `flatten_spec_into`, once
    /// `Self` + this spec's own generics are already bound in a pushed scope
    /// there. Always empty for a non-alias declaration.
    pub fn resolve_spec_dependencies(
        &mut self,
        sp: &HirSpecDef,
    ) -> Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)> {
        let module = self.module_path.clone();
        sp.dependencies
            .iter()
            .filter_map(|dep| self.resolve_spec_dependency_cell(sp.id, sp.span, dep, false, &module))
            .collect()
    }

    /// The cell-only half of resolving one raw dependency `Type` -- see
    /// `resolve_spec_dependencies`'s doc comment for why the args stay
    /// unresolved here. `spec_declaration`'s own cache is deliberately
    /// accessor-blind (one canonical cell shared by every caller, see its
    /// doc comment) -- unlike the ordinary `resolve_item` path, it performs
    /// no visibility check of its own, so the accessor-aware check has to
    /// be re-run here by hand, through the same `check_visibility` choke
    /// point every other in-analyzer visibility check already goes through.
    ///
    /// `ambient_fallback` retries against `ModuleResolver::
    /// ambient_core_candidates` when the primary lookup misses, mirroring
    /// `Context::resolve_generic_type`'s identical retry for ordinary
    /// *type-position* references -- needed by
    /// `resolve_raw_spec_fn_type`'s `Type::SpecStatic` case (`false` for
    /// `resolve_spec_dependencies` above, unchanged): a `core`-declared
    /// spec's own `spec T` return bound is flattened from an *implementor's*
    /// module context, not `core`'s own, so an ambiently-resolvable bound
    /// name (`Iterator`) would otherwise only resolve correctly from inside
    /// `core` itself -- almost never true in practice.
    fn resolve_spec_dependency_cell(
        &mut self,
        id: HirId,
        span: Span,
        ty: &Type,
        ambient_fallback: bool,
        module: &[Ident],
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)> {
        let (path, raw_args) = match ty {
            Type::Generic(path, args) => (path, args.clone()),
            Type::Named(path) => (path, vec![]),
            _ => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(Ident(
                        "<spec>".to_string(),
                    ))),
                );
                return None;
            }
        };
        let absolute = match self.context.resolve_absolute_item_path(
            &mut *self.resolver,
            path,
            module,
        ) {
            Ok(a) => a,
            Err(e) => {
                self.error(id, span, AnalysisErrorKind::UnresolvedType(e));
                return None;
            }
        };
        let primary = match self.resolver.spec_declaration(&absolute) {
            Ok(found) => found,
            Err(e) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(e));
                return None;
            }
        };
        let not_a_spec = || TypeResolutionError::NotASpec(path.head.clone());
        let cell = if let Some(cell) = primary {
            cell
        } else if ambient_fallback && path.is_unqualified() {
            let ambient_path = match self
                .resolver
                .ambient_core_candidates(module, &path.head)
            {
                Ok(Some(ambient_path)) => ambient_path,
                Ok(None) => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(not_a_spec()));
                    return None;
                }
                Err(e) => {
                    self.error(id, span, AnalysisErrorKind::ModuleResolution(e));
                    return None;
                }
            };
            match self.resolver.spec_declaration(&ambient_path) {
                Ok(Some(cell)) => cell,
                _ => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(not_a_spec()));
                    return None;
                }
            }
        } else {
            self.error(id, span, AnalysisErrorKind::UnresolvedType(not_a_spec()));
            return None;
        };
        let (visibility, declaring_module) = {
            let c = cell.borrow();
            (c.visibility, c.module_path.clone())
        };
        if !self.check_visibility(visibility, &declaring_module) {
            self.error(
                id,
                span,
                AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                    module: declaring_module,
                    item: cell.borrow().name.clone(),
                }),
            );
            return None;
        }
        Some((cell, raw_args))
    }

    /// The ambiently-resolvable `core::iterator::{name}` spec cell, used by
    /// `Analyzer::for_in_source_declares` for conform-registry identity
    /// comparison. Tries this
    /// module's own implicit absolute path first (`[self.module_path,
    /// name]`, or a real import alias if one exists), then falls back to
    /// `ModuleResolver::ambient_core_candidates` -- the same two-step retry
    /// `Context::resolve_generic_type` already gives every *type-position*
    /// reference to a `core`-exposed name (a conform declaration or
    /// generic bound); this is the one caller that needs the identical
    /// fallback from a for-in-loop's own value-analysis-time context
    /// instead, which never goes through `resolve_type` at all. `None` for
    /// anything that isn't a clean single-candidate resolution -- missing,
    /// broken, *or* ambiguous -- callers degrade to "not iterable" rather
    /// than a bespoke diagnostic either way, matching this function's
    /// existing best-effort contract.
    fn resolve_ambient_iterator_spec_cell(
        &mut self,
        name: &str,
    ) -> Option<Rc<RefCell<ResolvedSpecType>>> {
        let name = Ident(name.to_string());
        let path = Path::from(name.clone());
        if let Ok(absolute) =
            self.context
                .resolve_absolute_item_path(&mut *self.resolver, &path, &self.module_path)
            && let Ok(Some(cell)) = self.resolver.spec_declaration(&absolute)
        {
            return Some(cell);
        }
        let ambient = self
            .resolver
            .ambient_core_candidates(&self.module_path, &name)
            .ok()
            .flatten()?;
        self.resolver.spec_declaration(&ambient).ok().flatten()
    }

    /// Whether `ty` has a registered conformance for the ambient iterator
    /// spec. Conformance metadata, rather than methods merged onto an
    /// aggregate cell, is the sole conformance source.
    pub(super) fn for_in_source_declares(&mut self, ty: &ResolvedType, name: &str) -> bool {
        !self.for_in_conformances(ty, name).is_empty()
    }

    /// Every direct registry entry for an ambient iterator spec. Unlike a
    /// receiver lookup, a `for` loop needs the spec arguments too: two
    /// `ToIterator<T>` implementations are only distinguishable by `T`.
    pub(super) fn for_in_conformances(
        &mut self,
        ty: &ResolvedType,
        name: &str,
    ) -> Vec<crate::resolved_type::ResolvedConformance> {
        let Some(target_cell) = self.resolve_ambient_iterator_spec_cell(name) else {
            return vec![];
        };
        match self.resolver.conformances_for_type(ty) {
            Ok(conformances) => conformances
                .into_iter()
                .filter(|conform| conform.spec.borrow().id == target_cell.borrow().id)
                .collect(),
            Err(_) => vec![],
        }
    }

    /// Resolves one spec function's raw signature against `substitution`
    /// (`Self` plus the spec's own generics, bound to concrete types).
    ///
    /// A `=> spec Bound<...>` return type (`Type::SpecStatic`) is special-
    /// cased: there is no concrete `ResolvedType` to resolve at all here
    /// (each implementor answers differently) -- `fn_type.return_type` is
    /// left as an inert `ResolvedType::Void` placeholder (never read; see
    /// `FlattenedSpecFn::return_type_bound`'s doc comment) and the real
    /// answer, `Bound`'s own resolved cell + type arguments, is returned
    /// alongside it instead.
    fn resolve_raw_spec_fn_type(
        &mut self,
        id: HirId,
        span: Span,
        raw: &RawSpecFunctionSig,
        substitution: &[(Ident, ResolvedType)],
        module: &[Ident],
    ) -> Option<(
        ResolvedFunctionType,
        Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    )> {
        self.with_substitution(substitution, |this| {
            let mut params = Vec::with_capacity(raw.params.len());
            let mut ok = true;
            for p in &raw.params {
                match this.resolve_type_or_error_in(id, span, &p.r#type, true, module) {
                    Some(r) => params.push((p.ident.clone(), r)),
                    None => ok = false,
                }
            }
            let mut return_type_bound = None;
            let return_type = match &raw.return_type {
                Type::SpecStatic(bound) => {
                    match this.resolve_spec_dependency_cell(id, span, bound, true, module) {
                        Some((cell, raw_args)) => {
                            let resolved_args: Option<Vec<ResolvedType>> = raw_args
                                .iter()
                                .map(|a| this.resolve_type_or_error_in(id, span, a, true, module))
                                .collect();
                            match resolved_args {
                                Some(args) => {
                                    return_type_bound = Some((cell, args));
                                    Some(ResolvedType::Void)
                                }
                                None => None,
                            }
                        }
                        None => None,
                    }
                }
                other => this.resolve_return_type_or_error_in(id, span, other, true, module),
            };
            if !ok {
                return None;
            }
            Some((
                ResolvedFunctionType {
                    params,
                    return_type: Box::new(return_type?),
                    is_variadic: raw.is_variadic,
                    self_mode: raw.self_mode,
                },
                return_type_bound,
            ))
        })
    }

    /// The full, ordered set of functions `spec<type_args>` requires from an
    /// implementor of type `self_type` -- walks the alias members
    /// depth-first (each member's own requirements appear before the next
    /// member's, and this spec's own functions come last, matching
    /// read-order intuition), substituting `Self -> self_type` and this
    /// spec's own generics -> `type_args` into every raw signature along the
    /// way. An ordinary (non-alias) spec's flatten is just its own function
    /// list.
    ///
    /// **Nothing is merged.** Two specs may declare the same name and
    /// signature, and those are different functions: every entry keeps its
    /// declaring spec's identity (`FlattenedSpecFn::spec_id` + its own type
    /// args), so `A::tag` and `B::tag` flatten side by side as two distinct
    /// requirements. The one remaining dedup is *identity* dedup: the same
    /// spec reaching the flatten twice (a diamond alias, `X = A + B; Y = A +
    /// C; Z = X + Y`) contributes each of `A`'s functions once, keyed on
    /// `(spec_id, type_args, name)`.
    ///
    /// This one ordered list is also dynamic dispatch's vtable slot order
    /// (`Codegen`'s vtable builder walks it identically) -- see
    /// [[omega-enums-design]]/the spec design plan for why one flattening
    /// serves both purposes. Because members are flattened block-by-block
    /// (never interleaved), the list is naturally **sectioned** per spec:
    /// an alias `AB = A + B`'s flatten is `[A's slots][B's slots]`, which is
    /// what makes a `spec *AB` object's narrowing cast (`<spec *A>x`) a
    /// constant offset onto the vtable.
    pub(super) fn flatten_spec(
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
        let (spec_id, spec_name, spec_visibility, spec_module, generics, dependencies, functions) = {
            let s = spec.borrow();
            (
                s.id,
                s.name.clone(),
                s.visibility,
                s.module_path.clone(),
                s.generics.clone(),
                s.dependencies.clone(),
                s.functions.clone(),
            )
        };

        let self_ident = Ident("Self".to_string());
        let substitution: Vec<(Ident, ResolvedType)> =
            std::iter::once((self_ident, self_type.clone()))
                .chain(generics.iter().cloned().zip(type_args.iter().cloned()))
                .collect();

        // Each alias member's own type args are still raw at this point (see
        // `ResolvedSpecType::dependencies`'s doc comment) -- resolved here,
        // now that `substitution` (this spec's own generics, now concrete)
        // is available, exactly the same `with_substitution` treatment
        // `resolve_raw_spec_fn_type` gives a raw function signature just
        // below. Everything a spec declares resolves against its *own*
        // module (`spec_module`), never the caller's -- the flatten runs in
        // whatever module happens to be asking, and definition-site
        // resolution is what makes a foreign spec's function types
        // (`fmt(*self, out: spec *mut Write)` in `std::io`) resolvable from
        // anywhere else.
        for (member_spec, member_raw_args) in &dependencies {
            let member_args: Vec<ResolvedType> = self.with_substitution(&substitution, |this| {
                member_raw_args
                    .iter()
                    .map(|a| this.resolve_type_or_error_in(id, span, a, true, &spec_module))
                    .collect::<Option<Vec<_>>>()
            })?;
            self.flatten_spec_into(id, span, member_spec, &member_args, self_type, out)?;
        }

        for (name, raw) in &functions {
            let (fn_type, return_type_bound) =
                self.resolve_raw_spec_fn_type(id, span, raw, &substitution, &spec_module)?;
            // Identity dedup only: the same spec, at the same type
            // arguments, contributing the same declaration twice (a diamond
            // alias). Same name from a *different* spec, or a different
            // instantiation, is a distinct requirement and is kept.
            if out.iter().any(|existing| {
                existing.spec_id == spec_id
                    && existing.type_args() == *type_args
                    && existing.name == *name
            }) {
                continue;
            }
            out.push(FlattenedSpecFn {
                name: name.clone(),
                fn_type,
                return_type_bound,
                raw: raw.clone(),
                spec_id,
                spec_name: spec_name.clone(),
                visibility: spec_visibility,
                substitution: substitution.clone(),
            });
        }
        Some(())
    }

    /// Whether a concrete method's own resolved signature (`own`) satisfies
    /// one requirement's signature (`req_fn_type` + `req_bound`) --
    /// `self_mode`/`is_variadic`/`params` always compare structurally, the
    /// same equality `ResolvedFunctionType` always used; the return type
    /// alone branches: ordinary equality when `req_bound` is `None` (100%
    /// of today's behavior, for the overwhelmingly common concrete-return
    /// requirement), or `own`'s own return type checked against the bound
    /// spec (`type_implements_spec`, recursively) when `Some` -- the
    /// associated-type-like case a `=> spec Bound<...>` requirement needs
    /// (see `FlattenedSpecFn::return_type_bound`'s doc comment).
    fn fn_satisfies_requirement(
        &mut self,
        id: HirId,
        span: Span,
        own: &ResolvedFunctionType,
        req_fn_type: &ResolvedFunctionType,
        req_bound: &Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    ) -> bool {
        match req_bound {
            None => own == req_fn_type,
            Some((spec, type_args)) => {
                own.self_mode == req_fn_type.self_mode
                    && own.is_variadic == req_fn_type.is_variadic
                    && own.params == req_fn_type.params
                    && self
                        .type_implements_spec(id, span, &own.return_type, spec, type_args, false)
                        .is_ok()
            }
        }
    }

    /// The spec ids reachable from `spec` through its alias-member list,
    /// including `spec` itself. Ids only: this is a membership test, so the
    /// per-member type arguments (raw at a declaration, see
    /// `ResolvedSpecType::dependencies`) never need resolving here. Only an
    /// *alias* ever has members; for an ordinary spec this is just `{spec}`.
    /// Public as the single shared implementation -- `omega_driver`'s
    /// `bound_context_for` membership test uses it too.
    pub fn alias_member_ids(spec: &Rc<RefCell<ResolvedSpecType>>, out: &mut HashSet<HirId>) {
        let (id, dependencies) = {
            let spec = spec.borrow();
            (spec.id, spec.dependencies.clone())
        };
        if !out.insert(id) {
            return;
        }
        for (member, _) in dependencies {
            Self::alias_member_ids(&member, out);
        }
    }

    /// Expands a declared bound set through every alias it names: for each
    /// bound, emit the `(spec.id, resolved args)` keys of the specs the
    /// bound actually *requires* -- every non-alias member, transitively --
    /// with a member's raw type arguments resolved under the *alias's* own
    /// generics bound to the bound's arguments (and `Self` bound to the
    /// bound's concrete type), against the alias's own module. Mirrors
    /// exactly what `flatten_spec_into` does for the same data, for the
    /// same reason: an alias is only a name for its members, so `T: AB`
    /// and `T: A + B` must be interchangeable everywhere the bound set is
    /// compared or tested for entailment (blanket precedence,
    /// derived-conformance admission).
    ///
    /// The alias's *own* id deliberately never appears: nothing ever
    /// conforms to an alias (`ConformToAliasSpec`), so it is never a
    /// distinct requirement -- only its leaves are. Including it would make
    /// `{AB, A, B}` and `{A, B}` compare as "more specific" instead of
    /// equal, the exact asymmetry this exists to remove.
    pub fn expand_bound_set(
        &mut self,
        id: HirId,
        span: Span,
        bounds: &[ResolvedBound],
    ) -> Vec<(HirId, Vec<ResolvedType>)> {
        let mut out: Vec<(HirId, Vec<ResolvedType>)> = Vec::new();
        for (concrete, spec, spec_args) in bounds {
            self.expand_bound_into(id, span, concrete, spec, spec_args, &mut out);
        }
        out
    }

    fn expand_bound_into(
        &mut self,
        id: HirId,
        span: Span,
        concrete: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
        out: &mut Vec<(HirId, Vec<ResolvedType>)>,
    ) {
        let (generics, dependencies, module) = {
            let s = spec.borrow();
            (
                s.generics.clone(),
                s.dependencies.clone(),
                s.module_path.clone(),
            )
        };
        if dependencies.is_empty() {
            let key = (spec.borrow().id, spec_args.to_vec());
            if !out.contains(&key) {
                out.push(key);
            }
            return;
        }
        let self_ident = Ident("Self".to_string());
        let substitution: Vec<(Ident, ResolvedType)> =
            std::iter::once((self_ident, concrete.clone()))
                .chain(generics.iter().cloned().zip(spec_args.iter().cloned()))
                .collect();
        for (member, member_raw_args) in &dependencies {
            let Some(member_args): Option<Vec<ResolvedType>> =
                self.with_substitution(&substitution, |this| {
                    member_raw_args
                        .iter()
                        .map(|a| this.resolve_type_or_error_in(id, span, a, true, &module))
                        .collect::<Option<Vec<_>>>()
                })
            else {
                // The member's own arguments failed to resolve -- already
                // reported; this bound simply contributes no expansion.
                continue;
            };
            self.expand_bound_into(id, span, concrete, member, &member_args, out);
        }
    }

    pub(super) fn type_implements_spec(
        &mut self,
        id: HirId,
        span: Span,
        ty: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_type_args: &[ResolvedType],
        _check_method_visibility: bool,
    ) -> Result<Vec<HirId>, Vec<Ident>> {
        match self.resolver.conformance_for(ty, spec, spec_type_args) {
            Ok(Some(conform)) => Ok(conform
                .methods
                .iter()
                .map(|(_, method)| method.decl_id)
                .collect()),
            // No entry registered under this exact spec. That does *not* mean
            // `ty` fails to implement it: a spec **alias** (`spec AB = A +
            // B`) is satisfied by conforming its *members*, and nobody ever
            // writes `conform T to AB` for one (it is rejected outright --
            // see `AnalysisErrorKind::ConformToAliasSpec`).
            //
            // So satisfy it the only way that is decidable here -- map
            // `spec`'s own flattened requirements onto the methods `ty`'s
            // registry entries already provide. Restricted to entries whose
            // spec is one of the alias's members, so an unrelated conform
            // that happens to share a signature can never contribute: this
            // is conformance, not method lookup, and it must not become a
            // second way for a foreign spec's method to count.
            //
            // The resulting order is `flatten_spec`'s, which is also the
            // vtable slot order (see `CheckedSpecCoerce::slots`).
            Ok(None) => {
                let Some(requirements) = self.flatten_spec(id, span, spec, spec_type_args, ty)
                else {
                    return Err(vec![]);
                };
                let mut permitted = HashSet::new();
                Self::alias_member_ids(spec, &mut permitted);
                let member_ids: Vec<HirId> = permitted.iter().copied().collect();
                let candidates = match self.resolver.conformances_for_specs(ty, &member_ids) {
                    Ok(entries) => entries,
                    Err(error) => {
                        self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                        return Err(vec![]);
                    }
                };
                // Each available method keeps its *entry's* spec identity
                // and type arguments -- a requirement is matched against
                // exactly the spec that declared it, so a same-named method
                // on an unrelated spec can never satisfy it (this is the
                // identity that used to be lost: `Ord`'s inline `equals`
                // stood in for `Eq`'s requirement, and the two mangled
                // differently).
                let available: Vec<(HirId, Vec<ResolvedType>, Ident, ResolvedMethod)> =
                    candidates
                        .into_iter()
                        .filter(|entry| permitted.contains(&entry.spec.borrow().id))
                        .flat_map(|entry| {
                            let spec_id = entry.spec.borrow().id;
                            let spec_args = entry.spec_args.clone();
                            entry
                                .methods
                                .into_iter()
                                .map(move |(name, method)| {
                                    (spec_id, spec_args.clone(), name, method)
                                })
                        })
                        .collect();

                let mut slots = Vec::with_capacity(requirements.len());
                let mut missing = Vec::new();
                for requirement in &requirements {
                    let found = available.iter().position(|(spec_id, spec_args, name, method)| {
                        *spec_id == requirement.spec_id
                            && *spec_args == requirement.type_args()
                            && *name == requirement.name
                            && self.fn_satisfies_requirement(
                                id,
                                span,
                                &method.fn_type,
                                &requirement.fn_type,
                                &requirement.return_type_bound,
                            )
                    });
                    match found {
                        Some(index) => slots.push(available[index].3.decl_id),
                        None => missing.push(requirement.name.clone()),
                    }
                }
                if missing.is_empty() { Ok(slots) } else { Err(missing) }
            }
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                Err(vec![])
            }
        }
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
    ) -> Option<Result<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>), (Ident, Vec<Ident>)>>
    {
        let (spec, spec_args) = self.resolve_spec_reference(id, span, bound)?;
        let spec_name = spec.borrow().name.clone();
        match self.type_implements_spec(id, span, concrete, &spec, &spec_args, false) {
            Ok(_) => Some(Ok((spec, spec_args))),
            Err(missing) => Some(Err((spec_name, missing))),
        }
    }
}
