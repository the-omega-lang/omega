use super::*;

/// What a `spec Name : Deps for Target { ... }` clause's `Target` resolves
/// to -- see `HirSpecDef::target`'s doc comment and
/// `Analyzer::resolve_extension_target`. `Concrete` is fully resolved and
/// ready to use immediately; `Pattern` (the one supported shape, `[?]T`)
/// defers resolution to a later, per-receiver call
/// (`Analyzer::resolve_extension_methods`), since there's no single
/// concrete instantiation to resolve eagerly.
#[derive(Debug, Clone)]
pub enum ExtensionTarget {
    Concrete(ResolvedType),
    Pattern,
}

/// What resolving an `implements` clause or a `for` block produces: the
/// methods to store on the implementor, every spec-default body still owed
/// a phase-2 check (see [`PendingSpecMethod`]), and every spec the clause
/// *nominally* named (each resolved to its cell + concrete type arguments)
/// -- the last is what `ResolvedStructType::implemented_specs` (and its
/// enum/union siblings) get patched with; see that field's own doc comment
/// for why this can't be reconstructed from the method list alone.
pub type SpecMethods =
    (Vec<(Ident, ResolvedMethod)>, Vec<PendingSpecMethod>, Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>);

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
        self.substitution[1..].iter().map(|(_, ty)| ty.clone()).collect()
    }
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
                self.error(id, span, AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name)));
                None
            }
        }
    }

    /// Whether `target` is the one supported pattern shape a `for` clause
    /// may use (`[?]T`, referencing the spec's own single generic parameter
    /// exactly) -- bare, with no leading `*`, exactly the same convention
    /// `for str { ... }` already uses: the target names the *value* type,
    /// and a function's own self-mode (`*self`/`*mut self`) is what adds
    /// the pointer, so `*self` inside a `for [?]T` block reads the same way
    /// `*self` inside a `for str` block already does -- "a pointer to the
    /// named target," not a pointer baked into the target spelling itself.
    /// Every self-mode restriction below still applies identically --
    /// bare `[?]T` is otherwise always-invalid syntax (`Context::
    /// resolve_type` rejects it unconditionally), this `for`-target
    /// position is the one dedicated exception, mirroring `str`'s own
    /// "never resolvable except via this raw-syntax special case" status.
    /// Shared between `resolve_extension_target` (which classifies a `for`
    /// clause) and `resolve_spec_functions` (which additionally restricts
    /// self-mode for it, see below).
    fn is_slice_extension_target(generics: &[Ident], target: &Type) -> bool {
        generics.len() == 1
            && matches!(
                target,
                Type::UnknownSizeArray(item) if matches!(
                    item.as_ref(),
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
    /// Also resolves and validates `sp.annotations` (`@gap`/`@suppress`) --
    /// folded in here, not a separate function, because this is the one
    /// place both of a spec's two possible resolution paths (an ordinary
    /// declaration, via `omega_driver::Driver::resolve_spec_declaration`,
    /// and a `for`-attached one, via `Analyzer::resolve_extension_methods`)
    /// already converge on exactly once -- see those two callers.
    /// `@gap`'s per-function self-lessness check belongs here for the same
    /// reason `ExtensionSelfMustBePointer`/`SpecSelfMustBePointer` already
    /// do: it's a per-function self-mode rule, checked in the same loop
    /// that already inspects every function's `self_mode` once.
    pub fn resolve_spec_functions(
        &mut self,
        sp: &HirSpecDef,
    ) -> (Vec<(Ident, RawSpecFunctionSig)>, crate::annotations::ResolvedAnnotations, Vec<(Ident, crate::resolved_type::GapFunction)>)
    {
        let annotations =
            crate::annotations::resolve(self, sp.id, &sp.annotations, crate::annotations::ItemKind::Spec, false, false);
        if annotations.gap {
            if sp.target.is_some() {
                self.error(sp.id, sp.span, AnalysisErrorKind::GapOnForSpec);
            }
            if sp.is_alias {
                self.error(sp.id, sp.span, AnalysisErrorKind::GapOnSpecAlias);
            }
            if !sp.generics.is_empty() {
                self.error(sp.id, sp.span, AnalysisErrorKind::GapMustNotBeGeneric);
            }
        }

        let generics: Vec<Ident> = sp.generics.iter().map(|g| g.ident.clone()).collect();
        let is_slice_extension =
            sp.target.as_ref().is_some_and(|t| Self::is_slice_extension_target(&generics, t));
        let mut functions = Vec::new();
        let mut gap_functions = Vec::new();
        let mut seen: HashSet<Ident> = HashSet::new();
        for f in &sp.functions {
            if !seen.insert(f.name.clone()) {
                self.error(f.id, f.span, AnalysisErrorKind::Redeclaration { name: f.name.clone(), previous: None });
                continue;
            }
            let by_value = matches!(f.self_mode, Some(SelfMode::Value) | Some(SelfMode::MutValue));
            if annotations.gap && f.self_mode.is_some() {
                self.error(f.id, f.span, AnalysisErrorKind::GapFunctionMustBeStatic { name: f.name.clone() });
            } else if annotations.gap && f.body.is_some() {
                self.error(f.id, f.span, AnalysisErrorKind::GapFunctionBodyNotYetSupported { name: f.name.clone() });
            } else if by_value && is_slice_extension {
                // `for [?]T`'s `self` (by value) resolves to
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

            // Only for a function that passed both gap checks above (no
            // `self`, no body) -- re-checking here (instead of a tracking
            // flag) is simplest since both conditions are already right
            // above and never change in between.
            if annotations.gap && f.self_mode.is_none() && f.body.is_none() {
                let params = self.analyze_all(&f.params, |this, p| {
                    this.resolve_type_or_error(p.id, p.span, &p.r#type, true).map(|t| (p.ident.clone(), t))
                });
                let return_type = self.resolve_return_type_or_error(f.id, f.span, &f.return_type, true);
                if let (Some(params), Some(return_type)) = (params, return_type) {
                    gap_functions.push((
                        f.name.clone(),
                        crate::resolved_type::GapFunction {
                            decl_id: f.id,
                            span: f.span,
                            fn_type: ResolvedFunctionType {
                                params,
                                return_type: Box::new(return_type),
                                is_variadic: false,
                                self_mode: None,
                            },
                        },
                    ));
                }
            }
        }
        (functions, annotations, gap_functions)
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
        let (functions, annotations, gap_functions) = self.resolve_spec_functions(sp);
        let generics: Vec<Ident> = sp.generics.iter().map(|g| g.ident.clone()).collect();
        // Never actually consulted -- a `for`-spec is never name-registered,
        // so `spec *Name` can never be written against this cell -- but kept
        // true anyway so this cell upholds the same invariant every other
        // `ResolvedSpecType` does (see its own doc comment).
        let is_object_safe = functions.iter().all(|(_, raw)| !matches!(raw.return_type, Type::SpecStatic(_)))
            && dependencies.iter().all(|(dep, _)| dep.borrow().is_object_safe);
        let cell = Rc::new(RefCell::new(ResolvedSpecType {
            id: sp.id,
            name: sp.name.clone(),
            visibility: sp.visibility,
            generics,
            module_path: self.module_path.clone(),
            type_args: vec![],
            is_object_safe,
            dependencies,
            functions,
            gap_functions,
            span: sp.span,
            // `@gap` on a `for`-spec is already rejected (`GapOnForSpec`,
            // resolved a moment ago inside `resolve_spec_functions`) --
            // `annotations.gap` is stored as-is anyway, matching
            // `is_object_safe`'s own "never actually consulted, but keep the
            // invariant honest" precedent just above.
            is_gap: annotations.gap,
            suppress: annotations.suppress,
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
                        return_type_bound: f.return_type_bound,
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
                            spec_type_args: f.type_args(),
                            function: f.name.clone(),
                        },
                    );
                }
            }
        }
        // A `for`-attached spec targets a primitive, which has no cell of
        // its own to store `implemented_specs` on -- never consulted for
        // this caller, so an empty list is fine (not a special case; every
        // other `SpecMethods` producer that has nowhere to put this would
        // do the same).
        Some((methods, pending, vec![]))
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
        sp.dependencies.iter().filter_map(|dep| self.resolve_spec_dependency_cell(sp.id, sp.span, dep, false)).collect()
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
            let ambient_path = match self.resolver.ambient_core_candidates(&self.module_path, &path.head) {
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

    /// The ambiently-resolvable `core::iterator::{name}` spec cell --
    /// `Analyzer::for_in_source_declares`'s one caller, used purely for
    /// cell-identity comparison against `ResolvedStructType::
    /// implemented_specs` (so no type-argument resolution/validation
    /// happens here at all, unlike `resolve_spec_reference`). Tries this
    /// module's own implicit absolute path first (`[self.module_path,
    /// name]`, or a real import alias if one exists), then falls back to
    /// `ModuleResolver::ambient_core_candidates` -- the same two-step retry
    /// `Context::resolve_generic_type` already gives every *type-position*
    /// reference to a `core`-exposed name (an `implements` clause, a
    /// generic bound); this is the one caller that needs the identical
    /// fallback from a for-in-loop's own value-analysis-time context
    /// instead, which never goes through `resolve_type` at all. `None` for
    /// anything that isn't a clean single-candidate resolution -- missing,
    /// broken, *or* ambiguous -- callers degrade to "not iterable" rather
    /// than a bespoke diagnostic either way, matching this function's
    /// existing best-effort contract.
    fn resolve_ambient_iterator_spec_cell(&mut self, name: &str) -> Option<Rc<RefCell<ResolvedSpecType>>> {
        let name = Ident(name.to_string());
        let path = Path::from(name.clone());
        if let Ok(absolute) = self.context.resolve_absolute_item_path(&mut *self.resolver, &path, &self.module_path)
            && let Ok(Some(cell)) = self.resolver.spec_declaration(&absolute)
        {
            return Some(cell);
        }
        let ambient = self.resolver.ambient_core_candidates(&self.module_path, &name).ok().flatten()?;
        self.resolver.spec_declaration(&ambient).ok().flatten()
    }

    /// Whether `ty` *nominally* declares `: {name}<AnyT>` in its own
    /// `implements` clause -- `Analyzer::analyze_for_in`'s real conformance
    /// check (against `ToIterator`, or `Iterator` directly -- see
    /// `for_in_source_kind`), replacing the duck-typed "does a method named
    /// `to_iterator`/`next` happen to resolve" the desugaring used to rely
    /// on exclusively. Deliberately reads `implemented_specs` (see its own
    /// doc comment for why this can't be `type_implements_spec`, which is
    /// structural) -- `false` for anything that isn't a struct/enum/union (a
    /// primitive has no `implements` clause of its own outside the separate
    /// `for`-attachment mechanism, out of scope here).
    pub(super) fn for_in_source_declares(&mut self, ty: &ResolvedType, name: &str) -> bool {
        let implemented = match ty {
            ResolvedType::Struct(cell) => cell.borrow().implemented_specs.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().implemented_specs.clone(),
            ResolvedType::Union(cell) => cell.borrow().implemented_specs.clone(),
            _ => return false,
        };
        let Some(target_cell) = self.resolve_ambient_iterator_spec_cell(name) else { return false };
        implemented.iter().any(|(spec, _)| spec.borrow().id == target_cell.borrow().id)
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
    ) -> Option<(ResolvedFunctionType, Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>)> {
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
                Type::SpecStatic(bound) => match this.resolve_spec_dependency_cell(id, span, bound, true) {
                    Some((cell, raw_args)) => {
                        let resolved_args: Option<Vec<ResolvedType>> =
                            raw_args.iter().map(|a| this.resolve_type_or_error(id, span, a, true)).collect();
                        match resolved_args {
                            Some(args) => {
                                return_type_bound = Some((cell, args));
                                Some(ResolvedType::Void)
                            }
                            None => None,
                        }
                    }
                    None => None,
                },
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
            let (fn_type, return_type_bound) = self.resolve_raw_spec_fn_type(id, span, raw, &substitution)?;
            if let Some(existing_index) = out.iter().position(|f| &f.name == name) {
                let existing = &out[existing_index];
                if !requirements_are_same(&existing.fn_type, &existing.return_type_bound, &fn_type, &return_type_bound)
                {
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

    /// Resolves a struct/enum/union's `implements` clause: flattens each
    /// declared spec *independently* (`flatten_spec` -- own internal
    /// name-based conflict detection fully intact, see `flatten_spec_into`'s
    /// doc comment for why that part stays as-is), then merges the results
    /// across entries keyed on the full `(name, signature)` pair, not the
    /// bare name -- two entries requiring the exact same signature under one
    /// name are still silently deduplicated (point 5 of the language
    /// design), but two requiring genuinely *different* signatures under one
    /// name (most commonly: the same generic spec implemented twice, at
    /// different type arguments -- `struct X : ToIterator<char>,
    /// ToIterator<*char>`) are now two independent requirements instead of a
    /// hard `ConflictingSpecFunctions` error, each satisfiable by its own
    /// overload exactly the way ordinary overloading already works for every
    /// other method here. For each requirement not already provided by
    /// `own_functions` (searched the same way: by exact `(name, signature)`,
    /// never name alone -- an overload that merely shares a name without
    /// matching a requirement's signature doesn't satisfy it, and must not
    /// shadow a *different* overload that does), either queues a
    /// default-method instantiation (spec supplied a body) or reports
    /// `MissingSpecFunction`. A satisfying method whose own visibility is
    /// *less* permissive than the spec function it's satisfying
    /// (`FlattenedSpecFn::visibility`) reports `SpecMethodTooHidden`
    /// instead -- an implementor can never narrow a spec's own contract,
    /// only match or widen it (see `omega_parser::ast::visibility::
    /// Visibility`'s ordering). Returns the additional `(name,
    /// ResolvedMethod)` entries to merge into the implementor's own
    /// `functions` list (already carrying freshly minted `decl_id`s) plus
    /// every queued default body still needing to be checked in phase 2.
    pub(super) fn resolve_implements_clause(
        &mut self,
        id: HirId,
        span: Span,
        implementor_name: &Ident,
        implements: &[Type],
        own_functions: &[(Ident, ResolvedMethod)],
        self_type: &ResolvedType,
        glue: bool,
    ) -> SpecMethods {
        let mut flattened: Vec<FlattenedSpecFn> = Vec::new();
        let mut implemented_specs = Vec::new();
        for spec_type in implements {
            let Some((spec, type_args)) = self.resolve_spec_reference(id, span, spec_type) else { continue };
            // A `@glue` marker may only implement gaps -- see
            // `AnalysisErrorKind::GlueOnNonGapSpec`'s doc comment for why
            // an ordinary spec is rejected here rather than silently
            // allowed alongside real gaps.
            if glue && !spec.borrow().is_gap {
                self.error(id, span, AnalysisErrorKind::GlueOnNonGapSpec { spec: spec.borrow().name.clone() });
            }
            implemented_specs.push((spec.clone(), type_args.clone()));
            // A conflict *within* this one entry's own dependency graph is
            // already reported inline (`ConflictingSpecFunctions`); `None`
            // just means skip this entry's remaining contribution.
            let Some(this_entry) = self.flatten_spec(id, span, &spec, &type_args, self_type) else { continue };
            for req in this_entry {
                if let Some(existing_index) = flattened.iter().position(|f| {
                    f.name == req.name
                        && requirements_are_same(&f.fn_type, &f.return_type_bound, &req.fn_type, &req.return_type_bound)
                }) {
                    // Exact duplicate (same name *and* signature), reached
                    // through two different entries (e.g. a shared
                    // dependency) -- silent dedup, same as within one
                    // entry's own flattening, except when the earlier
                    // occurrence was a bare requirement and this one brings
                    // an actual default: see `flatten_spec_into`'s identical
                    // "later default wins" reasoning.
                    if flattened[existing_index].raw.default_body.is_none() && req.raw.default_body.is_some() {
                        flattened[existing_index] = req;
                    }
                    continue;
                }
                flattened.push(req);
            }
        }

        let mut extra_methods = Vec::new();
        let mut pending = Vec::new();
        for req in flattened {
            let satisfying_index = own_functions.iter().position(|(name, own)| {
                *name == req.name
                    && self.fn_satisfies_requirement(id, span, &own.fn_type, &req.fn_type, &req.return_type_bound)
            });
            if let Some(index) = satisfying_index {
                let own = &own_functions[index].1;
                if own.visibility < req.visibility {
                    self.error(
                        id,
                        span,
                        AnalysisErrorKind::SpecMethodTooHidden {
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
                        return_type_bound: req.return_type_bound,
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
                            spec_type_args: req.type_args(),
                            function: req.name.clone(),
                        },
                    );
                }
            }
        }
        (extra_methods, pending, implemented_specs)
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
    /// `HirSpecDef::target`'s doc comment). On success, returns each
    /// satisfying method's own `decl_id`, one per required slot, in
    /// `flatten_spec`'s deterministic order -- this is also the exact vtable
    /// slot order `Codegen::vtable_for` needs, precomputed here (where a
    /// resolver/`find_methods` is available) so codegen never has to
    /// re-derive "which concrete method satisfies which slot" by matching
    /// names on its own (a match that, post-`resolve_implements_clause`'s
    /// exact-signature fix, a bare name is no longer enough to make
    /// correctly -- see `CheckedSpecCoerce::slots`). Returns the missing
    /// function names on failure instead -- an insufficiently-*visible*
    /// satisfying method (only possible when `check_method_visibility` is
    /// set) is folded into the same "missing" list, not a separate error
    /// shape: from this caller's own perspective the two are equivalent
    /// ("you can't treat `ty` as implementing `spec` from here"), matching
    /// `coerce_to_expected`'s own accepted "no bespoke diagnostic per
    /// coercion site" simplification.
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
    /// no concrete method left to check). A `Hidden` method satisfying a
    /// `Hidden` spec would otherwise leak: an implementor's own `Hidden`
    /// method is scoped to its *owning type's* method bodies (narrower --
    /// see `check_member_visibility`), while a `Hidden` spec is scoped to
    /// its whole *declaring module* (wider) -- so "the spec is visible
    /// here" no longer implies "this satisfying method would be too."
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
                    && self.type_implements_spec(id, span, &own.return_type, spec, type_args, false).is_ok()
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
        check_method_visibility: bool,
    ) -> Result<Vec<HirId>, Vec<Ident>> {
        let Some(required) = self.flatten_spec(id, span, spec, spec_type_args, ty) else {
            return Err(vec![]);
        };
        let (owner_module_path, owner_id) = ty.declaring_owner().unwrap_or_else(|| (Vec::new(), id));
        let mut missing = Vec::new();
        let mut slots = Vec::with_capacity(required.len());
        for req in &required {
            let candidates = self.find_methods(id, span, ty, &req.name);
            let Some(method) = candidates
                .into_iter()
                .find(|m| self.fn_satisfies_requirement(id, span, &m.fn_type, &req.fn_type, &req.return_type_bound))
            else {
                missing.push(req.name.clone());
                continue;
            };
            if check_method_visibility && !self.check_member_visibility(method.visibility, &owner_module_path, owner_id)
            {
                missing.push(req.name.clone());
                continue;
            }
            slots.push(method.decl_id);
        }
        if missing.is_empty() { Ok(slots) } else { Err(missing) }
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
            Ok(_) => Some(Ok(())),
            Err(missing) => Some(Err((spec_name, missing))),
        }
    }
}
