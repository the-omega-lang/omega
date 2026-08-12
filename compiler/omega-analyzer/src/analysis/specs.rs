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
    pub(super) spec_name: Ident,
    /// The visibility of whichever spec directly declares this function
    /// (`spec_name`'s own `ResolvedSpecType::visibility`) -- a spec
    /// function has no per-function modifier of its own (see
    /// `SpecFunctionStmt`), it always inherits its declaring spec's. This
    /// is inherited by the composed method. Tracked per function rather than
    /// read once off the top-level compose target, because a dependency spec
    /// (`spec Mammal : Animal`) can have
    /// a different visibility than the spec that depends on it -- each
    /// function keeps the visibility of the spec that actually declared
    /// it, exactly the same way `spec_name` already does.
    pub(super) visibility: Visibility,
    /// `Self` + the owning spec's own generics, bound to concrete types --
    /// exactly what resolved `fn_type` above, kept around so a queued
    /// default instantiation's *body* can be checked later (phase 2, see
    /// `PendingSpecMethod`) with the identical substitution its signature
    /// already used.
    pub(super) substitution: Vec<(Ident, ResolvedType)>,
}

impl FlattenedSpecFn {
    /// `spec_name`'s own concrete type arguments -- `substitution` always
    /// starts with `("Self", self_type)` followed by the declaring spec's
    /// generics in declaration order (see where `substitution` is built in
    /// `Analyzer::flatten_spec_into`), so this is just that leading `Self`
    /// entry dropped. Lets a diagnostic naming `spec_name` (e.g.
    /// `MissingSpecFunction`) show *which* instantiation it's about --
    /// `"Consumer<*u8>"`, not just `"Consumer"` -- when the same spec is
    /// implemented more than once at different type arguments.
    pub(super) fn type_args(&self) -> Vec<ResolvedType> {
        self.substitution[1..]
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect()
    }
}

/// A spec-default method an implementor needs (no override, spec supplied
/// a body) -- signature already resolved and merged into the implementor's
/// composed method list in phase 1; this is what phase 2 still needs to
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

/// Whether two requirements under the same name are the *same* requirement
/// (safe to silently dedup, per point 5 of the language design) rather than
/// a genuine conflict -- `self_mode`/`is_variadic`/`params` always compare
/// structurally, exactly as plain `ResolvedFunctionType` equality already
/// did; only the return type's comparison branches: ordinary `ResolvedType`
/// equality when neither side is `spec`-bound (100% of today's behavior),
/// or same-bound-spec-and-args when both are, via `ResolvedSpecType`'s own
/// nominal (id-based) `PartialEq`. A `SpecBound` paired with a `Concrete`
/// requirement under the same name is never considered the same (a real
/// conflict, reported as `ConflictingSpecFunctions`).
fn requirements_are_same(
    a_fn_type: &ResolvedFunctionType,
    a_bound: &Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    b_fn_type: &ResolvedFunctionType,
    b_bound: &Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
) -> bool {
    match (a_bound, b_bound) {
        (None, None) => a_fn_type == b_fn_type,
        (Some((spec_a, args_a)), Some((spec_b, args_b))) => {
            a_fn_type.self_mode == b_fn_type.self_mode
                && a_fn_type.is_variadic == b_fn_type.is_variadic
                && a_fn_type.params == b_fn_type.params
                && *spec_a.borrow() == *spec_b.borrow()
                && args_a == args_b
        }
        _ => false,
    }
}

impl<'r> Analyzer<'r> {
    pub fn resolve_compose_target(
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
        if let Type::UnknownSizeArray(item) = target {
            let item = self.resolve_type_or_error(id, span, item, true)?;
            return Some(ResolvedType::Slice {
                item: Box::new(item),
                mutable: false,
            });
        }
        self.resolve_type_or_error(id, span, target, true)
    }

    pub fn check_compose_block(
        &mut self,
        id: HirId,
        span: Span,
        target: &ResolvedType,
        spec_type: &Type,
        functions: &[HirFunctionDef],
        inherent: &[(Ident, ResolvedMethod)],
    ) -> Option<(
        Rc<RefCell<ResolvedSpecType>>,
        Vec<ResolvedType>,
        Vec<(Ident, ResolvedMethod)>,
        Vec<PendingSpecMethod>,
    )> {
        let (spec, spec_args) = self.resolve_spec_reference(id, span, spec_type)?;
        let requirements = self.flatten_spec(id, span, &spec, &spec_args, target)?;
        let requirement_names: HashSet<Ident> = requirements
            .iter()
            .map(|requirement| requirement.name.clone())
            .collect();

        self.context.enter_scope();
        let signatures = self.analyze_all(functions, |this, function| {
            this.collect_function_signature(function, None)
        });
        self.context.leave_scope();
        let signatures = signatures?;
        self.check_overload_duplicates(functions, &signatures);

        let source = ComposeSource {
            spec: spec.clone(),
            spec_args: spec_args.clone(),
        };
        let mut methods = Vec::with_capacity(requirements.len());
        let mut pending = Vec::new();
        for requirement in requirements {
            let matching = functions
                .iter()
                .zip(&signatures)
                .find(|(function, (signature, _))| {
                    function.name == requirement.name
                        && self.fn_satisfies_requirement(
                            id,
                            span,
                            signature,
                            &requirement.fn_type,
                            &requirement.return_type_bound,
                        )
                });
            if let Some((function, (signature, annotations))) = matching {
                methods.push((
                    requirement.name.clone(),
                    ResolvedMethod {
                        decl_id: function.id,
                        fn_type: signature.clone(),
                        visibility: requirement.visibility,
                        annotations: annotations.clone(),
                        source: Some(source.clone()),
                    },
                ));
            } else if let Some((_, method)) = inherent.iter().find(|(name, method)| {
                *name == requirement.name
                    && self.fn_satisfies_requirement(
                        id,
                        span,
                        &method.fn_type,
                        &requirement.fn_type,
                        &requirement.return_type_bound,
                    )
            }) {
                let mut method = method.clone();
                method.visibility = requirement.visibility;
                methods.push((requirement.name.clone(), method));
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
        for function in functions {
            if !methods
                .iter()
                .any(|(name, method)| *name == function.name && method.decl_id == function.id)
                && !requirement_names.contains(&function.name)
            {
                self.error(
                    function.id,
                    function.span,
                    AnalysisErrorKind::ComposeExtraFunction {
                        spec: spec_name.clone(),
                        function: function.name.clone(),
                    },
                );
            }
        }
        Some((spec, spec_args, methods, pending))
    }
    /// Resolves a raw `Type` that's expected to name a spec (a compose target,
    /// spec dependency, or generic bound) to its cell plus
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
    /// between *dependencies* is a `flatten_spec`-time concern instead
    /// (only detectable once both sides are resolved with a concrete
    /// `Self`).
    /// Also resolves `sp.annotations` -- folded in here, not a separate
    /// function, because this is the one
    /// place both of a spec's two possible resolution paths (an ordinary
    /// declaration, via `omega_driver::Driver::resolve_spec_declaration`,
    /// and a `for`-attached one, via `Analyzer::resolve_extension_methods`)
    /// already converge on exactly once -- see those two callers.
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
                // program later). A `for`-attached spec is exempt: it has
                // no name, so it can never appear in `spec *T` position at
                // all.
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
                    params: f.params.clone(),
                    return_type: f.return_type.clone(),
                    default_body: f.body.clone(),
                },
            ));
        }
        (functions, annotations)
    }

    /// Resolves a spec's own declared dependency list (`spec Mammal :
    /// Animal, Dummy`) to their cells, keeping each dependency's own type
    /// arguments **raw** (unresolved `Type`, not `ResolvedType`). Unlike
    /// `resolve_spec_reference` (used wherever a concrete reference's args
    /// are already resolvable -- a generic bound or compose declaration),
    /// this runs at the *depending* spec's own declaration, before its own
    /// generics are ever bound to anything concrete -- resolving a
    /// dependency's args here would fail for exactly the case that matters
    /// (`spec Foo<T> : Bar<T>`), the same way `resolve_spec_functions`
    /// already stays raw for the identical reason. Only *which* spec each
    /// dependency names is resolved eagerly here, via `ModuleResolver::
    /// spec_declaration` (an args-independent lookup); the args themselves
    /// are resolved later, in `flatten_spec_into`, once `Self` + this
    /// spec's own generics are already bound in a pushed scope there.
    pub fn resolve_spec_dependencies(
        &mut self,
        sp: &HirSpecDef,
    ) -> Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)> {
        sp.dependencies
            .iter()
            .filter_map(|dep| self.resolve_spec_dependency_cell(sp.id, sp.span, dep, false))
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
            &self.module_path,
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
                .ambient_core_candidates(&self.module_path, &path.head)
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
    /// `Analyzer::for_in_source_declares` for compose-registry identity
    /// comparison. Tries this
    /// module's own implicit absolute path first (`[self.module_path,
    /// name]`, or a real import alias if one exists), then falls back to
    /// `ModuleResolver::ambient_core_candidates` -- the same two-step retry
    /// `Context::resolve_generic_type` already gives every *type-position*
    /// reference to a `core`-exposed name (a compose declaration or
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

    /// Whether `ty` has a registered composition for the ambient iterator
    /// spec. Composition metadata, rather than methods merged onto an
    /// aggregate cell, is the sole conformance source.
    pub(super) fn for_in_source_declares(&mut self, ty: &ResolvedType, name: &str) -> bool {
        let Some(target_cell) = self.resolve_ambient_iterator_spec_cell(name) else {
            return false;
        };
        match self.resolver.composes_for_type(ty) {
            Ok(composes) => composes
                .iter()
                .any(|compose| compose.spec.borrow().id == target_cell.borrow().id),
            Err(_) => false,
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
    ) -> Option<(
        ResolvedFunctionType,
        Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    )> {
        self.with_substitution(substitution, |this| {
            let mut params = Vec::with_capacity(raw.params.len());
            let mut ok = true;
            for p in &raw.params {
                match this.resolve_type_or_error(id, span, &p.r#type, true) {
                    Some(r) => params.push((p.ident.clone(), r)),
                    None => ok = false,
                }
            }
            let mut return_type_bound = None;
            let return_type = match &raw.return_type {
                Type::SpecStatic(bound) => {
                    match this.resolve_spec_dependency_cell(id, span, bound, true) {
                        Some((cell, raw_args)) => {
                            let resolved_args: Option<Vec<ResolvedType>> = raw_args
                                .iter()
                                .map(|a| this.resolve_type_or_error(id, span, a, true))
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
                other => this.resolve_return_type_or_error(id, span, other, true),
            };
            if !ok {
                return None;
            }
            Some((
                ResolvedFunctionType {
                    params,
                    return_type: Box::new(return_type?),
                    is_variadic: false,
                    self_mode: raw.self_mode,
                },
                return_type_bound,
            ))
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
        let (spec_name, spec_visibility, generics, dependencies, functions) = {
            let s = spec.borrow();
            (
                s.name.clone(),
                s.visibility,
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

        // Each dependency's own type args are still raw at this point (see
        // `ResolvedSpecType::dependencies`'s doc comment) -- resolved here,
        // now that `substitution` (this spec's own generics, now concrete)
        // is available, exactly the same `with_substitution` treatment
        // `resolve_raw_spec_fn_type` gives a raw function signature just
        // below.
        for (dep_spec, dep_raw_args) in &dependencies {
            let dep_args: Vec<ResolvedType> = self.with_substitution(&substitution, |this| {
                dep_raw_args
                    .iter()
                    .map(|a| this.resolve_type_or_error(id, span, a, true))
                    .collect::<Option<Vec<_>>>()
            })?;
            self.flatten_spec_into(id, span, dep_spec, &dep_args, self_type, out)?;
        }

        for (name, raw) in &functions {
            let (fn_type, return_type_bound) =
                self.resolve_raw_spec_fn_type(id, span, raw, &substitution)?;
            if let Some(existing_index) = out.iter().position(|f| &f.name == name) {
                let existing = &out[existing_index];
                if !requirements_are_same(
                    &existing.fn_type,
                    &existing.return_type_bound,
                    &fn_type,
                    &return_type_bound,
                ) {
                    self.error(
                        id,
                        span,
                        AnalysisErrorKind::ConflictingSpecFunctions {
                            name: name.clone(),
                            first_spec: existing.spec_name.clone(),
                            second_spec: spec_name.clone(),
                        },
                    );
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
                        return_type_bound,
                        raw: raw.clone(),
                        spec_name: spec_name.clone(),
                        visibility: spec_visibility,
                        substitution: substitution.clone(),
                    };
                }
                continue;
            }
            out.push(FlattenedSpecFn {
                name: name.clone(),
                fn_type,
                return_type_bound,
                raw: raw.clone(),
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

    pub(super) fn type_implements_spec(
        &mut self,
        id: HirId,
        span: Span,
        ty: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_type_args: &[ResolvedType],
        _check_method_visibility: bool,
    ) -> Result<Vec<HirId>, Vec<Ident>> {
        match self.resolver.compose_for(ty, spec, spec_type_args) {
            Ok(Some(compose)) => Ok(compose
                .methods
                .iter()
                .map(|(_, method)| method.decl_id)
                .collect()),
            Ok(None) => {
                let requirements = self
                    .flatten_spec(id, span, spec, spec_type_args, ty)
                    .unwrap_or_default();
                let composes = match self.resolver.composes_for_type(ty) {
                    Ok(composes) => composes,
                    Err(error) => {
                        self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                        return Err(vec![]);
                    }
                };
                let methods: Vec<_> = composes
                    .into_iter()
                    .flat_map(|compose| compose.methods)
                    .collect();
                let mut slots = Vec::with_capacity(requirements.len());
                let mut missing = Vec::new();
                for requirement in requirements {
                    if let Some((_, method)) = methods.iter().find(|(name, method)| {
                        *name == requirement.name
                            && self.fn_satisfies_requirement(
                                id,
                                span,
                                &method.fn_type,
                                &requirement.fn_type,
                                &requirement.return_type_bound,
                            )
                    }) {
                        slots.push(method.decl_id);
                    } else {
                        missing.push(requirement.name);
                    }
                }
                if missing.is_empty() {
                    Ok(slots)
                } else {
                    Err(missing)
                }
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
