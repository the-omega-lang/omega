use super::*;

/// What a `spec Name : Deps for Target { ... }` clause's `Target` resolves
/// to -- see `HirSpecDef::target`'s doc comment and
/// `Analyzer::resolve_extension_target`. `Concrete` is fully resolved and
/// ready to use immediately; `Pattern` (the one supported shape, `[T]`)
/// defers resolution to a later, per-receiver call
/// (`Analyzer::resolve_extension_methods`), since there's no single
/// concrete instantiation to resolve eagerly.
#[derive(Debug, Clone)]
pub enum ExtensionTarget {
    Concrete(ResolvedType),
    Pattern,
}

/// What resolving an `implements` clause or a `for` block produces: the
/// methods to store on the implementor, paired with every spec-default body
/// still owed a phase-2 check (see [`PendingSpecMethod`]).
pub type SpecMethods = (Vec<(Ident, ResolvedMethod)>, Vec<PendingSpecMethod>);

/// One spec function requirement, flattened out of a (possibly generic,
/// possibly multiply-inherited) spec reference and resolved for one
/// specific concrete implementor -- see `Analyzer::flatten_spec`.
pub(super) struct FlattenedSpecFn {
    pub(super) name: Ident,
    pub(super) fn_type: ResolvedFunctionType,
    pub(super) raw: RawSpecFunctionSig,
    pub(super) spec_name: Ident,
    /// The visibility of whichever spec directly declares this function
    /// (`spec_name`'s own `ResolvedSpecType::visibility`) -- a spec
    /// function has no per-function modifier of its own (see
    /// `SpecFunctionStmt`), it always inherits its declaring spec's. This
    /// is the *minimum* visibility an implementor's own satisfying method
    /// must have -- see `Analyzer::resolve_implements_clause`. Tracked per
    /// function rather than read once off the top-level `implements`
    /// target, because a dependency spec (`spec Mammal : Animal`) can have
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

impl<'r> Analyzer<'r> {
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
                self.error(id, span, AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name)));
                None
            }
        }
    }

    /// Whether `target` is the one supported pattern shape a `for` clause
    /// may use (`[T]`, referencing the spec's own single generic parameter
    /// exactly) -- shared between `resolve_extension_target` (which
    /// classifies a `for` clause) and `resolve_spec_functions` (which
    /// additionally restricts self-mode for it, see below).
    fn is_slice_extension_target(generics: &[Ident], target: &Type) -> bool {
        generics.len() == 1
            && matches!(
                target,
                Type::Array(inner) if matches!(
                    inner.as_ref(),
                    Type::Named(path) if path.is_unqualified() && path.head == generics[0]
                )
            )
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
        let generics: Vec<Ident> = sp.generics.iter().map(|g| g.ident.clone()).collect();
        let is_slice_extension =
            sp.target.as_ref().is_some_and(|t| Self::is_slice_extension_target(&generics, t));
        let mut functions = Vec::new();
        let mut seen: HashSet<Ident> = HashSet::new();
        for f in &sp.functions {
            if !seen.insert(f.name.clone()) {
                self.error(f.id, f.span, AnalysisErrorKind::Redeclaration { name: f.name.clone(), previous: None });
                continue;
            }
            let by_value = matches!(f.self_mode, Some(SelfMode::Value) | Some(SelfMode::MutValue));
            if by_value && is_slice_extension {
                // `for [T]`'s `self` (by value) resolves to
                // `ResolvedType::Array` -- an unsized, lengthless thin
                // pointer, not the lengthed `Slice` `*self` gives -- see
                // `AnalysisErrorKind::ExtensionSelfMustBePointer`'s doc
                // comment.
                self.error(f.id, f.span, AnalysisErrorKind::ExtensionSelfMustBePointer { name: f.name.clone() });
            } else if by_value && sp.target.is_none() {
                // `spec *T` dynamic dispatch erases `Self` down to a bare
                // data pointer (see `finish_dynamic_dispatch_call`) -- a
                // by-value self has no way to survive that, so it's
                // rejected here, unconditionally, rather than only where a
                // `spec *T` coercion actually happens (a spec used only for
                // static bounds today could gain one anywhere else in the
                // program later). A `for`-attached spec is exempt: it has
                // no name, so it can never appear in `spec *T` position at
                // all (see `HirSpecDef::target`'s doc comment).
                self.error(f.id, f.span, AnalysisErrorKind::SpecSelfMustBePointer { name: f.name.clone() });
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
        functions
    }

    /// Every `ResolvedType` a `for` clause may concretely target -- the
    /// built-in scalar/`str` set `Context::new()` seeds `defined_types`
    /// with, plus `str` itself (via the same bare-`str` carve-out `*str`
    /// already relies on, see `resolve_extension_target`). Deliberately
    /// excludes anything struct/enum/union/spec-shaped -- `for` exists to
    /// give *primitives* a method table, not as a second, out-of-line way
    /// to implement a spec for an ordinary declared type.
    fn is_extendable_primitive(ty: &ResolvedType) -> bool {
        matches!(
            ty,
            ResolvedType::Void
                | ResolvedType::Bool
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
                | ResolvedType::Str { .. }
        )
    }

    /// Resolves and validates one `for`-spec's target (see
    /// `HirSpecDef::target`'s doc comment) -- enforces both `for`-specific
    /// rules right here, at the spec's own declaration, rather than
    /// deferred to first use: declared inside the module tree rooted at
    /// `core` (`self.module_path`, already the querying spec's own module),
    /// and targeting only an allowed primitive or the one supported pattern
    /// shape (`is_slice_extension_target`). `None` on any failure (already
    /// recorded as an `AnalysisError`) or when `sp.target` is itself `None`
    /// (an ordinary spec -- not a `for`-spec at all).
    pub fn resolve_extension_target(&mut self, sp: &HirSpecDef) -> Option<ExtensionTarget> {
        let target = sp.target.as_ref()?;
        if self.module_path.first().map(Ident::as_ref) != Some("core") {
            self.error(sp.id, sp.span, AnalysisErrorKind::ExtensionOutsideCore { name: sp.name.clone() });
            return None;
        }
        let generics: Vec<Ident> = sp.generics.iter().map(|g| g.ident.clone()).collect();
        if type_references_generics(&generics, target) {
            if Self::is_slice_extension_target(&generics, target) {
                return Some(ExtensionTarget::Pattern);
            }
            self.error(sp.id, sp.span, AnalysisErrorKind::ExtensionTargetNotAllowed { name: sp.name.clone() });
            return None;
        }
        // Same bare-`str` carve-out `Context::resolve_type`'s own `*str`
        // handling relies on (see its doc comment): `"str"` is deliberately
        // never registered in `defined_types`, so it has to be recognized
        // here, from the raw syntax, before an ordinary resolve attempt.
        let is_bare_str = matches!(
            target,
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "str"
        );
        let resolved = if is_bare_str {
            ResolvedType::Str { mutable: false }
        } else {
            self.resolve_type_or_error(sp.id, sp.span, target, false)?
        };
        if !Self::is_extendable_primitive(&resolved) {
            self.error(sp.id, sp.span, AnalysisErrorKind::ExtensionTargetNotAllowed { name: sp.name.clone() });
            return None;
        }
        Some(ExtensionTarget::Concrete(resolved))
    }

    /// Resolves a `for`-spec's own functions against a concrete `receiver`
    /// -- reuses the exact same dependency-flattening/implements-
    /// satisfaction machinery an ordinary struct's `implements` clause
    /// already goes through (`flatten_spec`), rather than a second,
    /// parallel implementation: a `for`-spec's own functions play both
    /// roles a struct's own methods usually split across two lists (the
    /// type's own declared functions, *and* whatever satisfies its
    /// `implements` clause), so they're fed into a throwaway, unregistered
    /// `ResolvedSpecType` cell built right here -- there is no existing
    /// cell to reuse, since `for`-specs are never name-registered (see
    /// `item_name`). `pattern_binding` is the one concrete type this
    /// specific `receiver` bound the spec's own single generic parameter to
    /// (see `resolve_extension_target`'s `Pattern` case) -- `None` for a
    /// `Concrete` target, which has no generics of its own to bind.
    ///
    /// Every flattened requirement (the spec's own functions, plus
    /// whatever its `: Deps` list additionally requires) must end up with a
    /// default body -- there is no separate "implementor" here who could
    /// supply one later, so any entry still missing one after flattening
    /// (a bare, unsatisfiable requirement, or a genuinely unimplemented
    /// dependency function) is reported via the same `MissingSpecFunction`
    /// diagnostic a struct's own unsatisfied `implements` clause would get.
    pub fn resolve_extension_methods(
        &mut self,
        sp: &HirSpecDef,
        receiver: &ResolvedType,
        pattern_binding: Option<ResolvedType>,
    ) -> Option<SpecMethods> {
        let dependencies = self.resolve_spec_dependencies(sp);
        let functions = self.resolve_spec_functions(sp);
        let generics: Vec<Ident> = sp.generics.iter().map(|g| g.ident.clone()).collect();
        let cell = Rc::new(RefCell::new(ResolvedSpecType {
            id: sp.id,
            name: sp.name.clone(),
            visibility: sp.visibility,
            generics,
            module_path: self.module_path.clone(),
            type_args: vec![],
            dependencies,
            functions,
        }));
        let type_args: Vec<ResolvedType> = pattern_binding.into_iter().collect();
        let flattened = self.flatten_spec(sp.id, sp.span, &cell, &type_args, receiver)?;

        let mut methods = Vec::with_capacity(flattened.len());
        let mut pending = Vec::new();
        for f in flattened {
            match &f.raw.default_body {
                Some(_) => {
                    let minted_id = self.resolver.fresh_synthetic_id();
                    methods.push((
                        f.name.clone(),
                        ResolvedMethod {
                            decl_id: minted_id,
                            fn_type: f.fn_type.clone(),
                            // A spec function has no visibility modifier of
                            // its own -- it inherits the visibility of
                            // whichever spec directly declares it (see
                            // `FlattenedSpecFn::visibility`). There's no
                            // separate implementor here to have made its own,
                            // possibly more permissive, choice (the "method"
                            // *is* the spec's own default body), so this is
                            // exactly that inherited visibility, not a
                            // hardcoded default.
                            visibility: f.visibility,
                            annotations: crate::annotations::ResolvedAnnotations::default(),
                        },
                    ));
                    pending.push(PendingSpecMethod {
                        id: minted_id,
                        fn_type: f.fn_type,
                        raw: f.raw,
                        substitution: f.substitution,
                    });
                }
                None => {
                    self.error(
                        sp.id,
                        sp.span,
                        AnalysisErrorKind::MissingSpecFunction {
                            implementor: Ident(receiver.to_string()),
                            spec: f.spec_name.clone(),
                            function: f.name.clone(),
                        },
                    );
                }
            }
        }
        Some((methods, pending))
    }

    /// Resolves a spec's own declared dependency list (`spec Mammal :
    /// Animal, Dummy`) to their cells, keeping each dependency's own type
    /// arguments **raw** (unresolved `Type`, not `ResolvedType`). Unlike
    /// `resolve_spec_reference` (used wherever a concrete reference's args
    /// are already resolvable -- a generic bound, an implements clause),
    /// this runs at the *depending* spec's own declaration, before its own
    /// generics are ever bound to anything concrete -- resolving a
    /// dependency's args here would fail for exactly the case that matters
    /// (`spec Foo<T> : Bar<T>`), the same way `resolve_spec_functions`
    /// already stays raw for the identical reason. Only *which* spec each
    /// dependency names is resolved eagerly here, via `ModuleResolver::
    /// spec_declaration` (an args-independent lookup); the args themselves
    /// are resolved later, in `flatten_spec_into`, once `Self` + this
    /// spec's own generics are already bound in a pushed scope there.
    pub fn resolve_spec_dependencies(&mut self, sp: &HirSpecDef) -> Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)> {
        sp.dependencies.iter().filter_map(|dep| self.resolve_spec_dependency_cell(sp.id, sp.span, dep)).collect()
    }

    /// The cell-only half of resolving one raw dependency `Type` -- see
    /// `resolve_spec_dependencies`'s doc comment for why the args stay
    /// unresolved here. `spec_declaration`'s own cache is deliberately
    /// accessor-blind (one canonical cell shared by every caller, see its
    /// doc comment) -- unlike the ordinary `resolve_item` path, it performs
    /// no visibility check of its own, so the accessor-aware check has to
    /// be re-run here by hand, through the same `check_visibility` choke
    /// point every other in-analyzer visibility check already goes through.
    fn resolve_spec_dependency_cell(
        &mut self,
        id: HirId,
        span: Span,
        ty: &Type,
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)> {
        let (path, raw_args) = match ty {
            Type::Generic(path, args) => (path, args.clone()),
            Type::Named(path) => (path, vec![]),
            _ => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(Ident("<spec>".to_string()))),
                );
                return None;
            }
        };
        let absolute = match self.context.resolve_absolute_item_path(&mut *self.resolver, path, &self.module_path) {
            Ok(a) => a,
            Err(e) => {
                self.error(id, span, AnalysisErrorKind::UnresolvedType(e));
                return None;
            }
        };
        let cell = match self.resolver.spec_declaration(&absolute) {
            Ok(Some(cell)) => cell,
            Ok(None) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(path.head.clone())),
                );
                return None;
            }
            Err(e) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(e));
                return None;
            }
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

    /// Resolves one spec function's raw signature against `substitution`
    /// (`Self` plus the spec's own generics, bound to concrete types).
    fn resolve_raw_spec_fn_type(
        &mut self,
        id: HirId,
        span: Span,
        raw: &RawSpecFunctionSig,
        substitution: &[(Ident, ResolvedType)],
    ) -> Option<ResolvedFunctionType> {
        self.with_substitution(substitution, |this| {
            let mut params = Vec::with_capacity(raw.params.len());
            let mut ok = true;
            for p in &raw.params {
                match this.resolve_type_or_error(id, span, &p.r#type, true) {
                    Some(r) => params.push((p.ident.clone(), r)),
                    None => ok = false,
                }
            }
            let return_type = this.resolve_type_or_error(id, span, &raw.return_type, true);
            if !ok {
                return None;
            }
            Some(ResolvedFunctionType {
                params,
                return_type: Box::new(return_type?),
                is_variadic: false,
                self_mode: raw.self_mode,
            })
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
            (s.name.clone(), s.visibility, s.generics.clone(), s.dependencies.clone(), s.functions.clone())
        };

        let self_ident = Ident("Self".to_string());
        let substitution: Vec<(Ident, ResolvedType)> = std::iter::once((self_ident, self_type.clone()))
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
                dep_raw_args.iter().map(|a| this.resolve_type_or_error(id, span, a, true)).collect::<Option<Vec<_>>>()
            })?;
            self.flatten_spec_into(id, span, dep_spec, &dep_args, self_type, out)?;
        }

        for (name, raw) in &functions {
            let fn_type = self.resolve_raw_spec_fn_type(id, span, raw, &substitution)?;
            if let Some(existing_index) = out.iter().position(|f| &f.name == name) {
                let existing = &out[existing_index];
                if existing.fn_type != fn_type {
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
                raw: raw.clone(),
                spec_name: spec_name.clone(),
                visibility: spec_visibility,
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
    /// -- it doesn't actually satisfy the contract; one whose signature
    /// matches but whose own visibility is *less* permissive than the spec
    /// function it's satisfying (`FlattenedSpecFn::visibility`) reports
    /// `SpecMethodTooPrivate` instead -- an implementor can never narrow a
    /// spec's own contract, only match or widen it (see
    /// `omega_parser::ast::visibility::Visibility`'s ordering). Returns the
    /// additional `(name, ResolvedMethod)` entries to merge into the
    /// implementor's own `functions` list (already carrying freshly minted
    /// `decl_id`s) plus every queued default body still needing to be
    /// checked in phase 2.
    pub(super) fn resolve_implements_clause(
        &mut self,
        id: HirId,
        span: Span,
        implementor_name: &Ident,
        implements: &[Type],
        own_functions: &[(Ident, ResolvedMethod)],
        self_type: &ResolvedType,
    ) -> SpecMethods {
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
                    self.error(
                        id,
                        span,
                        AnalysisErrorKind::MissingSpecFunction {
                            implementor: implementor_name.clone(),
                            spec: req.spec_name.clone(),
                            function: req.name.clone(),
                        },
                    );
                } else if own.visibility < req.visibility {
                    self.error(
                        id,
                        span,
                        AnalysisErrorKind::SpecMethodTooPrivate {
                            implementor: implementor_name.clone(),
                            spec: req.spec_name.clone(),
                            function: req.name.clone(),
                            required: req.visibility,
                            found: own.visibility,
                        },
                    );
                }
                continue;
            }
            match &req.raw.default_body {
                Some(_) => {
                    let minted_id = self.resolver.fresh_synthetic_id();
                    // A spec-default method carries no annotations of its
                    // own -- not yet part of the spec-function grammar (see
                    // `check_pending_spec_method`'s identical `attributes:
                    // Vec::new()` on its synthetic `HirFunctionDef`).
                    extra_methods.push((
                        req.name.clone(),
                        ResolvedMethod {
                            decl_id: minted_id,
                            fn_type: req.fn_type.clone(),
                            // Same reasoning as `resolve_extension_methods`'s
                            // identical case -- a spec-default method has no
                            // visibility modifier of its own, so it inherits
                            // its declaring spec's (`req.visibility`, see
                            // `FlattenedSpecFn::visibility`). There's no
                            // implementor override here to have chosen
                            // something more permissive.
                            visibility: req.visibility,
                            annotations: crate::annotations::ResolvedAnnotations::default(),
                        },
                    ));
                    pending.push(PendingSpecMethod {
                        id: minted_id,
                        fn_type: req.fn_type,
                        raw: req.raw,
                        substitution: req.substitution,
                    });
                }
                None => {
                    self.error(
                        id,
                        span,
                        AnalysisErrorKind::MissingSpecFunction {
                            implementor: implementor_name.clone(),
                            spec: req.spec_name.clone(),
                            function: req.name.clone(),
                        },
                    );
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
    /// confirms it. For a struct/enum/union this is purely a caller-mistake
    /// check (a spec-bound generic instantiated with a type that never
    /// declared `: Spec`); for a primitive it's the one thing that makes a
    /// `for`-attached spec's own `: Deps` list actually count as satisfying
    /// `Deps` for generic-bound purposes too (`find_methods`'s primitive
    /// fallback is what supplies a real answer here now -- see
    /// `HirSpecDef::target`'s doc comment). Returns the missing function
    /// names on failure -- an insufficiently-*visible* satisfying method
    /// (only possible when `check_method_visibility` is set) is folded into
    /// the same "missing" list, not a separate error shape: from this
    /// caller's own perspective the two are equivalent ("you can't treat
    /// `ty` as implementing `spec` from here"), matching `coerce_to_expected`'s
    /// own accepted "no bespoke diagnostic per coercion site" simplification.
    ///
    /// `check_method_visibility` exists because this function serves two
    /// genuinely different callers: `check_generic_bound` (a `T: Animal`
    /// bound is a structural fact about `T`, independent of who's asking --
    /// a generic body's own `self.speak()`-style calls are separately,
    /// correctly visibility-checked at each real call site) passes `false`;
    /// `coerce_to_expected` passes `true`, because *that* call site is the
    /// one place a concrete type's own method identity is erased into an
    /// opaque `spec *T` handle -- once erased, `finish_dynamic_dispatch_call`
    /// never re-checks visibility at all (by design: erasure means there's
    /// no concrete method left to check). A `Private` method satisfying a
    /// `Private` spec would otherwise leak: an implementor's own `Private`
    /// method is scoped to its *owning type's* method bodies (narrower --
    /// see `check_member_visibility`), while a `Private` spec is scoped to
    /// its whole *declaring module* (wider) -- so "the spec is visible
    /// here" no longer implies "this satisfying method would be too."
    pub(super) fn type_implements_spec(
        &mut self,
        id: HirId,
        span: Span,
        ty: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_type_args: &[ResolvedType],
        check_method_visibility: bool,
    ) -> Result<(), Vec<Ident>> {
        let Some(required) = self.flatten_spec(id, span, spec, spec_type_args, ty) else {
            return Err(vec![]);
        };
        let (owner_module_path, owner_id) = ty.declaring_owner().unwrap_or_else(|| (Vec::new(), id));
        let missing: Vec<Ident> = required
            .iter()
            .filter(|req| {
                let Some(method) =
                    self.find_methods(id, span, ty, &req.name).into_iter().find(|m| m.fn_type == req.fn_type)
                else {
                    return true;
                };
                check_method_visibility && !self.check_member_visibility(method.visibility, &owner_module_path, owner_id)
            })
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
        match self.type_implements_spec(id, span, concrete, &spec, &spec_args, false) {
            Ok(()) => Some(Ok(())),
            Err(missing) => Some(Err((spec_name, missing))),
        }
    }
}
