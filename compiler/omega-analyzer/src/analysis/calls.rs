use super::*;

/// One call-shape interceptor's answer.
///
/// The three interceptors below each peek at a call's shape and decide
/// whether it is theirs to resolve. `Declined` means "not my shape, try the
/// next one" -- deliberately distinct from claiming the call and failing,
/// which must *not* fall through to another interpretation of the same
/// source (its own error was already reported).
pub(super) enum Intercepted {
    Declined,
    Claimed(Option<CheckedExprNode>),
}

/// One call-shape interceptor: peeks at a call and either claims it or
/// declines. Written as a function pointer so the interceptors can be listed
/// in priority order at the call site.
pub(super) type Interceptor<'r> =
    fn(&mut Analyzer<'r>, HirId, Span, &HirFunctionCall) -> Intercepted;

/// A member call's receiver (`base` in `base.method(args)`), resolved once:
/// the place it came from -- needed to check writability and to de-assume a
/// narrowing for a `mut self` call -- the checked place itself, what it
/// resolves to, and whether it is writable.
struct Receiver {
    place: HirPlace,
    checked: CheckedPlace,
    r#type: ResolvedType,
    mutable: bool,
}

/// A function-call's callee, resolved to either an ordinary value (whose
/// type must be `Function`) or a bound method reference (a "thiscall"):
/// `base.method(args)` where `method` names a struct method rather than a
/// field becomes an ordinary call to the method with `&base` (or, if `base`
/// was already a pointer, `base` itself) prepended as the first (`self`)
/// argument -- `HirFunctionDef`'s synthetic `self` parameter (see
/// `omega_hir::lower::lower_function_def`) already accounts for it in
/// `fn_type`, so no special-casing is needed in the argument-checking loop
/// in `FunctionCall` handling.
pub(super) struct ResolvedCallee {
    pub(super) callee: CheckedExprNode,
    pub(super) fn_type: ResolvedFunctionType,
    pub(super) implicit_self: Option<CheckedExprNode>,
    /// `Some` only when `method` named 2+ overloaded candidates -- overload
    /// resolution (`Analyzer::resolve_overload`) already had to fully
    /// analyze (and pick the concrete type of) every user-written argument
    /// itself, to score candidates, so those are handed back here instead
    /// of asking `FunctionCall`'s own argument loop to redo (and
    /// potentially re-error on) the same work. `None` -- the overwhelming
    /// majority of calls -- means the ordinary loop runs exactly as before.
    pub(super) checked_args: Option<Vec<CheckedExprNode>>,
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
pub(super) enum CalleeResolution {
    Ordinary(ResolvedCallee),
    Dynamic(Option<CheckedExprNode>),
}

impl<'r> Analyzer<'r> {
    /// An absolute item path split into the item's own name and its owning
    /// module -- `None` only for an empty path, which no caller can produce.
    fn split_item_path(absolute: &[Ident]) -> Option<(Ident, Vec<Ident>)> {
        absolute
            .split_last()
            .map(|(name, module)| (name.clone(), module.to_vec()))
    }

    /// The plain path a call's callee names, or `None` when this call isn't
    /// one of the shapes the interceptors below can claim: a method call, a
    /// call through a computed expression, or a callee carrying explicit
    /// generic arguments (which already pin their own instantiation, so
    /// nothing here needs to deduce one).
    fn callee_path(call: &HirFunctionCall) -> Option<&Path> {
        let HirExpr::Place(place) = &Self::strip_reveal(&call.callee).1.expr else {
            return None;
        };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else {
            return None;
        };
        expr_path.plain()
    }

    /// The checked node for a call to an already-decided function: the
    /// callee reference itself, wrapped in the call. Shared by every path
    /// that resolves a callee by name rather than by expression (overloads,
    /// static overloads, generic instantiation).
    fn checked_call(
        &self,
        node_id: HirId,
        span: Span,
        callee_site: &HirExprNode,
        decl_id: HirId,
        storage: Storage,
        fn_type: ResolvedFunctionType,
        args: Vec<CheckedExprNode>,
    ) -> CheckedExprNode {
        let function = ResolvedType::Function(fn_type.clone());
        let callee = CheckedExprNode {
            id: callee_site.id,
            span: callee_site.span,
            r#type: function.clone(),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable {
                    decl_id,
                    storage,
                    r#type: function,
                },
                projections: vec![],
            }),
        };
        CheckedExprNode {
            id: node_id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(callee),
                fn_type,
                args,
            }),
        }
    }

    /// `Spec::method(receiver, ...)` is the explicit escape hatch for a
    /// composed instance method outside a bound context. The first argument
    /// selects the target composition; the resulting call is an ordinary
    /// direct call to that composition's symbol.
    pub(super) fn resolve_spec_qualified_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Intercepted {
        let HirExpr::Place(callee_place) = &Self::strip_reveal(&call.callee).1.expr else {
            return Intercepted::Declined;
        };
        if !callee_place.projections.is_empty() {
            return Intercepted::Declined;
        }
        let HirPlaceRoot::Path(expr_path) = &callee_place.root else {
            return Intercepted::Declined;
        };
        let path = &expr_path.path;
        let segments = path.segments();
        let Some((method_name, spec_segments)) = segments.split_last() else {
            return Intercepted::Declined;
        };
        if spec_segments.is_empty() {
            return Intercepted::Declined;
        }
        if !expr_path.generic_args.is_empty() && expr_path.args_at + 1 != spec_segments.len() {
            return Intercepted::Declined;
        }
        let spec_path = Path {
            head: spec_segments[0].clone(),
            tail: spec_segments[1..].to_vec(),
        };
        let absolute = match self.context.resolve_absolute_item_path(
            &mut *self.resolver,
            &spec_path,
            &self.module_path,
        ) {
            Ok(absolute) => absolute,
            Err(_) => return Intercepted::Declined,
        };
        let spec = match self.resolver.spec_declaration(&absolute) {
            Ok(Some(spec)) => spec,
            _ if spec_path.is_unqualified() => {
                let ambient = match self
                    .resolver
                    .ambient_core_candidates(&self.module_path, &spec_path.head)
                {
                    Ok(Some(ambient)) => ambient,
                    _ => return Intercepted::Declined,
                };
                match self.resolver.spec_declaration(&ambient) {
                    Ok(Some(spec)) => spec,
                    _ => return Intercepted::Declined,
                }
            }
            _ => return Intercepted::Declined,
        };
        let spec_args = if expr_path.generic_args.is_empty() {
            Vec::new()
        } else {
            let Some(args) = self.resolve_generic_arg_list(node_id, span, expr_path) else {
                return Intercepted::Claimed(None);
            };
            args
        };
        let Some(first) = call.args.first() else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: 1,
                    found: 0,
                },
            );
            return Intercepted::Claimed(None);
        };
        let Some(first_checked) = self.analyze_expr(first, None) else {
            return Intercepted::Claimed(None);
        };
        let target = first_checked.r#type.autoderef().clone();
        let compose_methods = match self.resolver.compose_for(&target, &spec, &spec_args) {
            Ok(Some(compose)) => compose.methods,
            Ok(None)
                if self
                    .type_implements_spec(node_id, span, &target, &spec, &spec_args, false)
                    .is_ok() =>
            {
                match self.resolver.composes_for_type(&target) {
                    Ok(composes) => composes
                        .into_iter()
                        .flat_map(|compose| compose.methods)
                        .collect(),
                    Err(err) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                        return Intercepted::Claimed(None);
                    }
                }
            }
            Ok(None) => {
                let missing = spec
                    .borrow()
                    .functions
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
                        type_name: target.to_string(),
                        spec: spec.borrow().name.clone(),
                        missing,
                    }),
                );
                return Intercepted::Claimed(None);
            }
            Err(err) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                return Intercepted::Claimed(None);
            }
        };
        let candidates: Vec<_> = compose_methods
            .into_iter()
            .filter(|(name, method)| name == method_name && method.fn_type.self_mode.is_some())
            .map(|(_, method)| method)
            .collect();
        if candidates.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec.borrow().name.clone(),
                    function: method_name.clone(),
                },
            );
            return Intercepted::Claimed(None);
        }
        let signatures: Vec<_> = candidates
            .iter()
            .map(|method| (method.decl_id, method.fn_type.clone()))
            .collect();
        let (winner, checked_args) = if candidates.len() == 1 {
            let method = &candidates[0];
            if call.args.len() != method.fn_type.params.len() {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::WrongArgumentCount {
                        expected: method.fn_type.params.len(),
                        found: call.args.len(),
                    },
                );
                return Intercepted::Claimed(None);
            }
            let mut checked_args = Vec::with_capacity(call.args.len());
            let mut ok = true;
            let adapted_first = if let HirExpr::Place(place) = &Self::strip_reveal(first).1.expr {
                let (checked, r#type, mutable) =
                    match self.analyze_place(first.id, first.span, place, None) {
                        Some(resolved) => resolved,
                        None => return Intercepted::Claimed(None),
                    };
                let receiver = Receiver {
                    place: place.clone(),
                    checked,
                    r#type,
                    mutable,
                };
                match self.adapt_self_argument(
                    &call.callee,
                    receiver,
                    method.fn_type.self_mode.expect("filtered above"),
                ) {
                    Some(adapted) => adapted,
                    None => return Intercepted::Claimed(None),
                }
            } else {
                first_checked.clone()
            };
            for (index, (arg, (_, expected))) in
                call.args.iter().zip(&method.fn_type.params).enumerate()
            {
                let checked = if index == 0 {
                    adapted_first.clone()
                } else if let Some(checked) = self.analyze_expr(arg, Some(expected)) {
                    checked
                } else {
                    ok = false;
                    continue;
                };
                let checked = self.coerce_to_expected(Some(expected), checked);
                if !expected.accepts(&checked.r#type) {
                    self.error(
                        arg.id,
                        arg.span,
                        AnalysisErrorKind::ArgumentTypeMismatch {
                            expected: expected.clone(),
                            found: checked.r#type.clone(),
                        },
                    );
                    ok = false;
                }
                checked_args.push(checked);
            }
            if !ok {
                return Intercepted::Claimed(None);
            }
            (0, checked_args)
        } else {
            let Some((winner, checked_args)) =
                self.resolve_overload(node_id, span, method_name, &signatures, &call.args)
            else {
                return Intercepted::Claimed(None);
            };
            (winner, checked_args)
        };
        let method = candidates[winner].clone();
        Intercepted::Claimed(Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            method.decl_id,
            Storage::Function,
            method.fn_type,
            checked_args,
        )))
    }

    /// What a call's callee expression resolves to.
    ///
    /// `base.field(args)` is the interesting shape: `field` may name a
    /// method (an implicit-`self` call), a callable field, or -- when `base`
    /// is a `spec *Spec` value -- a dynamically dispatched spec function.
    /// Everything else is an ordinary expression whose type must be a
    /// function.
    pub(super) fn resolve_callee(
        &mut self,
        callee: &HirExprNode,
        args: &[HirExprNode],
    ) -> Option<CalleeResolution> {
        // `reveal` is fully transparent, so every use of `callee` below
        // (including its own `id`/`span`) sees through it. `was_reveal` feeds
        // `with_reveal_bypass` at this path's own visibility checks -- there
        // is no enclosing `analyze_expr` `Reveal` arm to rely on here (see
        // `strip_reveal`).
        let (was_reveal, callee) = Self::strip_reveal(callee);

        let member = match &callee.expr {
            HirExpr::Place(place) => match place.projections.last() {
                Some(HirProjection::FieldAccess(field)) => Some((place, field)),
                _ => None,
            },
            _ => None,
        };
        let Some((place, field)) = member else {
            let checked = self.analyze_expr(callee, None)?;
            let fn_type = self.require_callable(callee.id, callee.span, checked.r#type.clone())?;
            return Some(CalleeResolution::Ordinary(ResolvedCallee {
                callee: checked,
                fn_type,
                implicit_self: None,
                checked_args: None,
            }));
        };

        let base_place = HirPlace {
            root: place.root.clone(),
            projections: place.projections[..place.projections.len() - 1].to_vec(),
        };
        let (checked, r#type, mutable) =
            self.analyze_place(callee.id, callee.span, &base_place, None)?;
        let receiver = Receiver {
            place: base_place,
            checked,
            r#type,
            mutable,
        };

        // Dynamic dispatch: the receiver is a `spec *Spec` value, not a
        // concrete type, so `field` is looked up in the spec's own flattened
        // function list -- there is no concrete `functions` list to consult,
        // the implementor is erased.
        if let ResolvedType::SpecObject {
            spec, type_args, ..
        } = &receiver.r#type
        {
            let (spec, type_args) = (spec.clone(), type_args.clone());
            return Some(CalleeResolution::Dynamic(
                self.finish_dynamic_dispatch_call(
                    callee.id,
                    callee.span,
                    receiver.checked,
                    &spec,
                    &type_args,
                    field,
                    args,
                ),
            ));
        }

        let methods = self.find_methods(callee.id, callee.span, &receiver.r#type, field);
        if methods.is_empty() {
            let receiver_type = receiver.r#type.autoderef();
            let field_shadows = match receiver_type {
                ResolvedType::Struct(cell) => cell
                    .borrow()
                    .fields
                    .iter()
                    .any(|(name, _, _)| name == field),
                ResolvedType::Union(cell) => cell
                    .borrow()
                    .fields
                    .iter()
                    .any(|(name, _, _)| name == field),
                ResolvedType::Enum { cell, variant } => {
                    let e = cell.borrow();
                    field.as_ref() == "tag"
                        || e.header.iter().any(|(name, _, _)| name == field)
                        || variant.is_some_and(|i| {
                            e.variants[i]
                                .fields
                                .iter()
                                .any(|(name, _, _)| name == field)
                        })
                }
                _ => false,
            };
            if !field_shadows {
                match self.resolver.composes_for_type(receiver_type) {
                    Ok(composes) => {
                        if let Some(compose) = composes.iter().find(|compose| {
                            compose.methods.iter().any(|(name, method)| {
                                name == field && method.fn_type.self_mode.is_some()
                            })
                        }) {
                            self.error(
                                callee.id,
                                callee.span,
                                AnalysisErrorKind::MethodNotInScope {
                                    method: field.clone(),
                                    spec: compose.spec.borrow().name.clone(),
                                },
                            );
                            return None;
                        }
                    }
                    Err(err) => {
                        self.error(
                            callee.id,
                            callee.span,
                            AnalysisErrorKind::ModuleResolution(err),
                        );
                        return None;
                    }
                }
            }
            return self.resolve_field_callee(callee, was_reveal, field, receiver);
        }
        self.resolve_method_callee(callee, was_reveal, field, receiver, methods, args)
    }

    /// A member call, `base.method(args)`: pick the method, check it is
    /// visible here, and adapt `base` into whatever its `self` parameter
    /// wants.
    fn resolve_method_callee(
        &mut self,
        callee: &HirExprNode,
        was_reveal: bool,
        field: &Ident,
        receiver: Receiver,
        methods: Vec<ResolvedMethod>,
        args: &[HirExprNode],
    ) -> Option<CalleeResolution> {
        let (method, checked_args) =
            self.pick_method(callee, field, &receiver.r#type, methods, args)?;
        self.require_method_visible(callee, was_reveal, field, &receiver.r#type, &method)?;

        // `self`'s own declared mode (`self`/`mut self`/`*self`/`*mut self`)
        // is read straight off the resolved signature, never
        // reverse-engineered from `params[0]`'s type shape.
        let self_mode = method
            .fn_type
            .self_mode
            .expect("a method resolved through find_methods always has a self mode");
        let self_arg = self.adapt_self_argument(callee, receiver, self_mode)?;

        let fn_type = ResolvedType::Function(method.fn_type.clone());
        let callee_expr = CheckedExprNode {
            id: callee.id,
            span: callee.span,
            r#type: fn_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable {
                    decl_id: method.decl_id,
                    storage: Storage::Function,
                    r#type: fn_type,
                },
                projections: vec![],
            }),
        };
        Some(CalleeResolution::Ordinary(ResolvedCallee {
            callee: callee_expr,
            fn_type: method.fn_type,
            implicit_self: Some(self_arg),
            checked_args,
        }))
    }

    /// Which of `methods` this call means. A function declared without
    /// `self` is static -- reached through the type's own name
    /// (`MyStruct::f()`), never through an instance -- so only the member
    /// candidates are callable here, and overload resolution never has to
    /// consider the static ones at all.
    ///
    /// The extra `Vec<CheckedExprNode>` is the already-analyzed arguments
    /// overload scoring had to produce; `None` when there was only one
    /// candidate and no scoring happened.
    fn pick_method(
        &mut self,
        callee: &HirExprNode,
        field: &Ident,
        receiver_type: &ResolvedType,
        methods: Vec<ResolvedMethod>,
        args: &[HirExprNode],
    ) -> Option<(ResolvedMethod, Option<Vec<CheckedExprNode>>)> {
        let members: Vec<ResolvedMethod> = methods
            .into_iter()
            .filter(|m| m.fn_type.self_mode.is_some())
            .collect();

        if members.len() > 1 {
            // Scored without each candidate's own synthesized `self` param:
            // `args` is the user-written arguments only, and `self` is never
            // itself overload-distinguishing (every member candidate has
            // exactly one, always viable). The winner's *real* (with-`self`)
            // signature is read back by index right after.
            let candidates: Vec<(HirId, ResolvedFunctionType)> = members
                .iter()
                .map(|m| {
                    let mut sans_self = m.fn_type.clone();
                    sans_self.params = sans_self.params[1..].to_vec();
                    (m.decl_id, sans_self)
                })
                .collect();
            let (winner, checked) =
                self.resolve_overload(callee.id, callee.span, field, &candidates, args)?;
            return Some((members[winner].clone(), Some(checked)));
        }
        if let Some(only) = members.into_iter().next() {
            return Some((only, None));
        }

        // Struct/union/enum have a real declared name; a primitive extension
        // target (reachable here only when a `for`-attached self-less
        // function shares a name with an instance call) has none, so its own
        // `Display` (`"u32"`, `"*[i32]"`, ...) stands in.
        let r#struct = match receiver_type.autoderef() {
            ResolvedType::Struct(cell) => cell.borrow().name.clone(),
            ResolvedType::Union(cell) => cell.borrow().name.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().name.clone(),
            other => Ident(other.to_string()),
        };
        self.error(
            callee.id,
            callee.span,
            AnalysisErrorKind::StaticFunctionOnInstance {
                r#struct,
                function: field.clone(),
            },
        );
        None
    }

    /// Rejects a call to a method the calling code isn't allowed to see.
    fn require_method_visible(
        &mut self,
        callee: &HirExprNode,
        was_reveal: bool,
        field: &Ident,
        receiver_type: &ResolvedType,
        method: &ResolvedMethod,
    ) -> Option<()> {
        // A primitive receiver's own methods can only come from a
        // `for`-attached spec, which is always `Exposed` -- the check below
        // is then trivially true whatever owner is passed.
        let (module_path, owner_id) = receiver_type
            .autoderef()
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), callee.id));
        let visible = self
            .with_reveal_bypass(was_reveal, callee.id, callee.span, |this| {
                Some(this.check_member_visibility(method.visibility, &module_path, owner_id))
            })
            .expect("the closure above always returns Some");
        if visible {
            return Some(());
        }
        self.error(
            callee.id,
            callee.span,
            AnalysisErrorKind::MethodNotVisible {
                method: field.clone(),
                base: receiver_type.clone(),
            },
        );
        None
    }

    /// Adapts the receiver to whatever `self_mode` declared, whichever shape
    /// the receiver itself happens to have -- a 2x2 matrix of (self wants a
    /// pointer / a value) x (receiver is a pointer / a value), plus the two
    /// already-fat-pointer receivers (`Str`/`Slice`) that need no wrapper of
    /// their own.
    fn adapt_self_argument(
        &mut self,
        callee: &HirExprNode,
        receiver: Receiver,
        self_mode: SelfMode,
    ) -> Option<CheckedExprNode> {
        let (id, span) = (callee.id, callee.span);
        let wants_mutable = self_mode.is_mutable();
        let Receiver {
            place,
            checked,
            r#type,
            mutable,
        } = receiver;

        // `comp_binding.method(...)` -- resolved separately, before any of
        // the ordinary arms below run: every one of them builds a
        // `CheckedExpr::Place`/`AddressOf` node from `checked`, which a
        // `Storage::Comp` place has no codegen meaning for (see
        // `Storage::Comp`'s own doc comment). See
        // `adapt_comp_self_argument`'s own doc comment for the substituted
        // counterpart.
        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = checked.root
        {
            return self.adapt_comp_self_argument(id, span, checked, r#type, self_mode);
        }

        let node = |r#type, kind| CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        };
        match (&r#type, self_mode.is_pointer()) {
            // self wants a pointer, the receiver already is one -- reuse it,
            // coerced to exactly the pointer shape self expects. That is what
            // a seamless deref would have produced anyway, so there is no
            // need to materialize a deref-then-address-of round trip just to
            // get the same pointer value back.
            (
                ResolvedType::Pointer {
                    pointee,
                    mutable: pointer_mutable,
                },
                true,
            ) => {
                self.require_mutable_pointer(id, span, wants_mutable, *pointer_mutable)?;
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(pointee.widened()),
                    mutable: wants_mutable,
                };
                Some(node(pointer, CheckedExpr::Place(checked)))
            }
            // self wants a pointer, the receiver is a `Str`/`Slice` value --
            // both already *are* their own fat-pointer representation (see
            // `Context::resolve_pointer_type`'s `*str`/`*[?]T` cases, which a
            // `for`-attached spec's own `Self` substitution goes through
            // identically), so an `AddressOf` wrapper here would add a
            // genuine extra indirection layer the signature never asked for.
            // Re-stamped with `self_mode`'s own mutability instead, mirroring
            // that same resolution rule on the call-site side.
            (
                ResolvedType::Str {
                    mutable: base_mutable,
                },
                true,
            ) => {
                self.require_mutable_pointer(id, span, wants_mutable, *base_mutable)?;
                Some(node(
                    ResolvedType::Str {
                        mutable: wants_mutable,
                    },
                    CheckedExpr::Place(checked),
                ))
            }
            (
                ResolvedType::Slice {
                    item,
                    mutable: base_mutable,
                },
                true,
            ) => {
                self.require_mutable_pointer(id, span, wants_mutable, *base_mutable)?;
                let slice = ResolvedType::Slice {
                    item: item.clone(),
                    mutable: wants_mutable,
                };
                Some(node(slice, CheckedExpr::Place(checked)))
            }
            // self wants a pointer, the receiver is a plain value -- auto-ref
            // (`&base`/`&mut base`).
            (_, true) => {
                if wants_mutable {
                    self.require_mutable_place(id, span, &place.root, &checked, mutable)?;
                    // De-assumption, exactly like an explicit `&mut`: a
                    // writable alias to the receiver exists for this call.
                    if let Some((ident, ..)) = self.narrowable_place(&place) {
                        self.context.widen_variable(&ident);
                    }
                }
                // Widened for the same reason an explicit `&`/`&mut` widens:
                // a method's `self` is `*Self`/`*mut Self`, never
                // `*Self::Variant`.
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(r#type.widened()),
                    mutable: wants_mutable,
                };
                Some(node(
                    pointer,
                    CheckedExpr::AddressOf(CheckedAddressOf { place: checked }),
                ))
            }
            // self wants a value, the receiver is a pointer -- auto-deref and
            // copy. No mutability check: reading through any pointer to
            // produce a copy is always legal, unlike writing through one.
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let pointee = pointee.widened();
                let mut place = checked;
                place.projections.push(CheckedProjection::Deref {
                    r#type: pointee.clone(),
                });
                Some(node(pointee, CheckedExpr::Place(place)))
            }
            // self wants a value and the receiver already is one -- passed
            // through unchanged; the copy happens naturally through ordinary
            // by-value argument passing. The receiver's own binding
            // mutability is irrelevant: mutating a `mut self` copy inside the
            // method never touches the caller's original.
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Place(checked))),
        }
    }

    /// `adapt_self_argument`'s `Storage::Comp` counterpart -- the same
    /// six-way shape (pointer-typed/`Str`/`Slice`/plain-value receiver,
    /// crossed with whether `self` wants a pointer or a value), just built
    /// from an already-known `ConstValue` (via `resolve_comp_place`)
    /// instead of a real place, since a `comp` binding has no address of
    /// its own to build `CheckedExpr::Place`/`AddressOf` from.
    ///
    /// `self` wants a pointer needs const promotion exactly like a bare
    /// `&comp_binding` does (see `analyze_address_of`'s identical case, and
    /// docs/19-compile-time-evaluation.md's "calling a method on a `comp`
    /// binding" section) -- except when the receiver's own type is already
    /// `Pointer`/`Str`/`Slice` (its own fat-pointer-shaped representation),
    /// in which case it's reused as-is, no extra indirection layered on.
    /// `self` wants a value needs no promotion at all: the already-known
    /// `ConstValue` is simply substituted in as an ordinary by-value
    /// argument, same as passing any other comp-computed value into any
    /// other function today.
    ///
    /// A `*mut self`/`&mut` is never legal here in any shape: once
    /// promoted (or, if already a pointer, wherever it already points),
    /// the data is real read-only rodata -- there is no writable storage a
    /// `comp` binding could ever hand out a mutable pointer into.
    fn adapt_comp_self_argument(
        &mut self,
        id: HirId,
        span: Span,
        checked: CheckedPlace,
        r#type: ResolvedType,
        self_mode: SelfMode,
    ) -> Option<CheckedExprNode> {
        if self_mode.is_pointer() && self_mode.is_mutable() {
            self.error(id, span, AnalysisErrorKind::NotMutablePointer);
            return None;
        }
        let value = self.resolve_comp_place(id, span, &checked)?;
        let node = |r#type, kind| CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        };
        match (&r#type, self_mode.is_pointer()) {
            // self wants a pointer, the receiver is already one -- only
            // reachable via `&<place>` *inside* an earlier `comp`
            // evaluation (see `ConstValue::Ref`'s own doc comment) --
            // reused as-is, widened to exactly the pointer shape self
            // expects.
            (ResolvedType::Pointer { pointee, .. }, true) => {
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(pointee.widened()),
                    mutable: false,
                };
                Some(node(pointer, CheckedExpr::Const(value)))
            }
            // self wants a pointer, the receiver is a `Str`/`Slice` value
            // -- already its own fat-pointer representation, exactly like
            // the ordinary path's identical arms, so used as-is.
            (ResolvedType::Str { .. }, true) => Some(node(
                ResolvedType::Str { mutable: false },
                CheckedExpr::Const(value),
            )),
            (ResolvedType::Slice { item, .. }, true) => {
                let slice = ResolvedType::Slice {
                    item: item.clone(),
                    mutable: false,
                };
                Some(node(slice, CheckedExpr::Const(value)))
            }
            // self wants a pointer, the receiver is a plain value -- const
            // promotion (see `analyze_address_of`'s identical case).
            (_, true) => {
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(r#type.widened()),
                    mutable: false,
                };
                Some(node(
                    pointer,
                    CheckedExpr::Const(ConstValue::Ref(Box::new(value))),
                ))
            }
            // self wants a value, the receiver is a pointer -- auto-deref:
            // unwrap the one `ConstValue::Ref` layer `&<place>` *inside*
            // the originating `comp` evaluation produced.
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let ConstValue::Ref(inner) = value else {
                    unreachable!(
                        "a comp value's own type is only ever Pointer alongside a ConstValue::Ref -- see ConstValue::Ref's doc comment"
                    );
                };
                Some(node(pointee.widened(), CheckedExpr::Const(*inner)))
            }
            // self wants a value and the receiver already is one --
            // substituted in unchanged, exactly like the ordinary path's
            // identical final arm.
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Const(value))),
        }
    }

    /// A `mut self` call needs a receiver that is writable *through the
    /// pointer it already is* -- an immutable pointer can never supply one.
    fn require_mutable_pointer(
        &mut self,
        id: HirId,
        span: Span,
        wants_mutable: bool,
        is_mutable: bool,
    ) -> Option<()> {
        if wants_mutable && !is_mutable {
            self.error(id, span, AnalysisErrorKind::NotMutablePointer);
            return None;
        }
        Some(())
    }

    /// `base.field(args)` where `field` is an ordinary (callable) field
    /// rather than a method. The already-resolved base place is finished in
    /// place instead of re-resolving the whole place from scratch, which
    /// would risk reporting the base's own errors (an undefined variable,
    /// say) twice.
    fn resolve_field_callee(
        &mut self,
        callee: &HirExprNode,
        was_reveal: bool,
        field: &Ident,
        receiver: Receiver,
    ) -> Option<CalleeResolution> {
        // `comp_binding.callable_field(...)` -- calling a function *value*
        // reached through a place, i.e. an indirect call, which is a
        // separate, deliberately still-open gap (unlike a plain method
        // call, resolved through `resolve_method_callee` instead -- see
        // `Analyzer::adapt_self_argument`'s own `Storage::Comp` handling).
        // Caught here, not in `resolve_callee`, so a comp-binding *method*
        // call isn't dragged down by the same restriction.
        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = receiver.checked.root
        {
            self.error(
                callee.id,
                callee.span,
                AnalysisErrorKind::CompEvalFailed {
                    reason:
                        "calling a function stored in a 'comp' binding's field isn't supported yet"
                            .into(),
                    trace: vec![],
                },
            );
            return None;
        }
        let CheckedPlace {
            root,
            mut projections,
        } = receiver.checked;
        let base_type = receiver.r#type;
        let field_type = self.with_reveal_bypass(was_reveal, callee.id, callee.span, |this| {
            this.resolve_field_projection(
                callee.id,
                callee.span,
                &mut projections,
                &base_type,
                field,
                &mut false,
            )
        })?;
        let checked = CheckedExprNode {
            id: callee.id,
            span: callee.span,
            r#type: field_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace { root, projections }),
        };
        let fn_type = self.require_callable(callee.id, callee.span, field_type)?;
        Some(CalleeResolution::Ordinary(ResolvedCallee {
            callee: checked,
            fn_type,
            implicit_self: None,
            checked_args: None,
        }))
    }

    /// Only a function can be called.
    fn require_callable(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
    ) -> Option<ResolvedFunctionType> {
        match r#type {
            ResolvedType::Function(fn_type) => Some(fn_type),
            _ => {
                self.error(id, span, AnalysisErrorKind::UnresolvedCallee);
                None
            }
        }
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
            self.error(
                id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec_name,
                    function: field.clone(),
                },
            );
            return None;
        };
        let fn_type = flattened[slot_index].fn_type.clone();
        // params[0] is the synthesized self -- never counted against the
        // user's own written arguments, exactly like an ordinary method
        // call's implicit self.
        let param_types = fn_type.params[1..].to_vec();

        if args.len() != param_types.len() {
            self.error(
                id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: param_types.len(),
                    found: args.len(),
                },
            );
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
                self.error(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: checked_arg.r#type.clone(),
                    },
                );
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
            kind: CheckedExpr::DynamicCall(CheckedDynamicCall {
                base,
                slot_index,
                fn_type,
                args: checked_args,
            }),
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
    pub(super) fn resolve_overloaded_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };
        let [member] = path.tail.as_slice() else {
            return Intercepted::Declined;
        };

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
            return Intercepted::Declined;
        }

        let r#type = if let Some(t) = self.context.find_defined_type(&path.head) {
            t.clone()
        } else if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
            t
        } else {
            let absolute: Vec<Ident> = self
                .module_path
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect();
            match self.resolve_item_checked(&absolute, &[], true) {
                Ok(ResolvedItem::Type(t)) => t,
                _ => return Intercepted::Declined,
            }
        };

        let all_methods = match &r#type {
            ResolvedType::Struct(cell) => cell.borrow().functions.clone(),
            ResolvedType::Union(cell) => cell.borrow().functions.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().functions.clone(),
            _ => return Intercepted::Declined,
        };
        let statics: Vec<ResolvedMethod> = all_methods
            .into_iter()
            .filter(|(name, m)| name == member && m.fn_type.self_mode.is_none())
            .map(|(_, m)| m)
            .collect();
        if statics.len() < 2 {
            return Intercepted::Declined;
        }

        let candidates: Vec<(HirId, ResolvedFunctionType)> = statics
            .iter()
            .map(|m| (m.decl_id, m.fn_type.clone()))
            .collect();
        let Some((winner, args)) =
            self.resolve_overload(node_id, span, member, &candidates, &call.args)
        else {
            return Intercepted::Claimed(None);
        };
        let (decl_id, fn_type) = candidates[winner].clone();

        // The winner's own visibility, checked *after* argument-driven
        // resolution picks it -- exactly like the single-candidate path
        // (`resolve_type_member`) already does; `resolve_overload` itself
        // has no notion of visibility, only signature fit.
        let (owner_module_path, owner_id) = r#type
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(statics[winner].visibility, &owner_module_path, owner_id) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: r#type.clone(),
                },
            );
            return Intercepted::Claimed(None);
        }

        Intercepted::Claimed(Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            Storage::Function,
            fn_type,
            args,
        )))
    }

    /// If `call`'s callee is `Owner::function(args)` where `Owner` is a
    /// generic struct/union/enum referenced with no explicit `<...>` and
    /// `function` names exactly one of its own non-overloaded, `self`-less
    /// (static) functions, infers `Owner`'s own omitted type arguments
    /// from the call's own argument types -- the same duck-typed
    /// unification a bare generic function call's arguments already get
    /// (`resolve_generic_call`), extended across the owner/function
    /// boundary. Declines (falls through to the ordinary path, unchanged)
    /// for every other shape: an already-concrete `Owner`, a
    /// module-qualified callee, 2+ overloaded static candidates under
    /// `function`'s name (`resolve_overloaded_static_call`'s own concern
    /// once `Owner` itself is concrete -- composing overload scoring with
    /// owner-generic inference at once is a separable follow-up, not
    /// attempted here), or a static function that declares independent
    /// generics of its own (matches `resolve_generic_call`'s own
    /// precedent of never attempting to infer a method-shaped generic
    /// call -- "struct generics are always explicit, so by the time a
    /// value of that type exists its methods are already fully
    /// monomorphized").
    pub(super) fn resolve_generic_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };
        let [member] = path.tail.as_slice() else {
            return Intercepted::Declined;
        };
        if self.context.find_defined_type(&path.head).is_some() {
            return Intercepted::Declined;
        }

        // Silent probe, like `resolve_overloaded_static_call`'s identical
        // alias check -- a real resolution failure here isn't this
        // function's to report; it's left for whichever fallback path
        // ends up actually needing this same alias to surface it.
        let alias = self.resolve_alias(&path.head).ok().flatten();
        let absolute: Vec<Ident> = match &alias {
            Some(ImportTarget::Item(absolute, _)) | Some(ImportTarget::GenericItem(absolute)) => {
                absolute.clone()
            }
            Some(ImportTarget::Module(_)) => return Intercepted::Declined,
            None => self
                .module_path
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect(),
        };

        let Some((real_absolute, sig)) = self.generic_static_function_signature_with_ambient(
            std::slice::from_ref(&path.head),
            &absolute,
            member,
        ) else {
            return Intercepted::Declined;
        };
        if !sig.function_generics.is_empty() {
            return Intercepted::Declined;
        }

        Intercepted::Claimed(self.finish_generic_static_call(
            node_id,
            span,
            call,
            std::slice::from_ref(&path.head),
            &real_absolute,
            member,
            &sig,
        ))
    }

    /// `resolver.generic_static_function_signature(absolute, function_name)`,
    /// retried against the `core` ambient fallback (see `ModuleResolver::
    /// ambient_core_candidates`) when `prefix` is a genuinely unqualified
    /// single segment and the direct lookup finds nothing there -- the same
    /// retry `Analyzer::generic_literal_signature_with_ambient` (literals.rs)
    /// already gives the literal-construction path, needed here for the
    /// identical reason (a bare, unimported ambient generic type's own
    /// static function must be discoverable too). Hands back whichever
    /// absolute path actually matched, since the final instantiation call
    /// needs the *real* declaration's path, not the naive own-module guess
    /// that failed to find anything locally.
    fn generic_static_function_signature_with_ambient(
        &mut self,
        prefix: &[Ident],
        absolute: &[Ident],
        function_name: &Ident,
    ) -> Option<(Vec<Ident>, GenericStaticFunctionSignature)> {
        if let Ok(Some(sig)) = self
            .resolver
            .generic_static_function_signature(absolute, function_name)
        {
            return Some((absolute.to_vec(), sig));
        }
        let [single] = prefix else { return None };
        let ambient = self
            .resolver
            .ambient_core_candidates(&self.module_path, single)
            .ok()
            .flatten()?;
        let sig = self
            .resolver
            .generic_static_function_signature(&ambient, function_name)
            .ok()
            .flatten()?;
        Some((ambient, sig))
    }

    /// The actual work behind `resolve_generic_static_call`, once it's
    /// confirmed `call`'s callee genuinely names a single-candidate static
    /// function on a generic type at `owner_absolute` -- split out so
    /// `resolve_generic_static_call` can stay a single check, mirroring
    /// `finish_generic_call`'s identical split for the free-function case.
    fn finish_generic_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        prefix: &[Ident],
        owner_absolute: &[Ident],
        member: &Ident,
        sig: &GenericStaticFunctionSignature,
    ) -> Option<CheckedExprNode> {
        let (checked_args, subst) = self.infer_generic_args(
            &sig.owner_generics,
            &sig.owner_defaults,
            &sig.params,
            &call.args,
        )?;

        let type_args =
            match resolve_inferred_type_args(&sig.owner_generics, &sig.owner_defaults, &subst) {
                Ok(type_args) => type_args,
                Err(_) => {
                    let missing: Vec<Ident> = sig
                        .owner_generics
                        .iter()
                        .zip(&sig.owner_defaults)
                        .filter(|(g, default)| default.is_none() && !subst.contains_key(*g))
                        .map(|(g, _)| g.clone())
                        .collect();
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedLiteralGeneric {
                            r#type: owner_absolute
                                .last()
                                .cloned()
                                .expect("an absolute path always has a last segment"),
                            generics: missing,
                        },
                    );
                    return None;
                }
            };

        let owner_type = match self.resolve_item_checked_with_ambient_fallback(
            prefix,
            owner_absolute,
            &type_args,
        ) {
            Ok(ResolvedItem::Type(t)) => t,
            Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                self.error(node_id, span, AnalysisErrorKind::UnresolvedCallee);
                return None;
            }
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                return None;
            }
        };

        let all_methods = match &owner_type {
            ResolvedType::Struct(cell) => cell.borrow().functions.clone(),
            ResolvedType::Union(cell) => cell.borrow().functions.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().functions.clone(),
            _ => unreachable!(
                "generic_static_function_signature only ever matches Struct/Union/Enum"
            ),
        };
        let method = all_methods
            .into_iter()
            .find(|(name, m)| name == member && m.fn_type.self_mode.is_none())
            .map(|(_, m)| m)
            .expect("generic_static_function_signature confirmed this static function exists");

        let (owner_module_path, owner_id) = owner_type
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(method.visibility, &owner_module_path, owner_id) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: owner_type.clone(),
                },
            );
            return None;
        }

        let ResolvedMethod {
            decl_id, fn_type, ..
        } = method;
        if checked_args.len() != fn_type.params.len() && !fn_type.is_variadic {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: fn_type.params.len(),
                    found: checked_args.len(),
                },
            );
            return None;
        }
        for (arg, (_, expected_type)) in checked_args.iter().zip(&fn_type.params) {
            if !expected_type.accepts(&arg.r#type) {
                self.error(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: arg.r#type.clone(),
                    },
                );
                return None;
            }
        }

        Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            Storage::Function,
            fn_type,
            checked_args,
        ))
    }

    /// If `ident` -- used unqualified, whether it's declared in this module
    /// or reached through a named import alias -- names an *overloaded*
    /// free function (2+ candidates), returns its real absolute path and
    /// its candidate list. For an aliased name, the list is filtered down
    /// to only the overloads this module can actually see, *unless* the
    /// import itself was written `reveal` (which brings every overload --
    /// visible or not -- into context; see `ModuleResolver::
    /// raw_import_absolute_path`'s doc comment). Own-module names are
    /// never filtered (a module always sees its own declarations, `reveal`
    /// or not -- filtering would be a no-op there anyway, since `Hidden`
    /// visibility already trivially allows same-module access).
    ///
    /// **`reveal` at the *use site* can no longer expand this set on its
    /// own** -- only `import reveal` can. Which overloads are even
    /// candidates has to be a fixed, resolution-time fact (the same way an
    /// ordinary, non-overloaded import's own target already is), not
    /// something a call-site `reveal` reaches into after the fact -- see
    /// `Analyzer::check_visibility`'s doc comment for the *different* rule
    /// that still applies to a module-*qualified* reference
    /// (`mymodule::thing::foo`, no alias involved at all), which keeps
    /// working with a use-site `reveal` exactly as before.
    ///
    /// `None` means "not an overloaded name at all" (0 or 1 real
    /// candidates) -- the caller falls through to the ordinary single-item
    /// path unchanged, exactly like `ModuleResolver::
    /// function_overload_signatures`'s own `Ok(None)` convention.
    pub(super) fn resolve_bare_overload_candidates(
        &mut self,
        ident: &Ident,
    ) -> Option<(Vec<Ident>, OverloadCandidates)> {
        let (absolute, is_alias, import_reveal) = match self
            .resolver
            .raw_import_absolute_path(&self.module_path, ident)
        {
            Ok(Some((absolute, reveal))) => (absolute, true, reveal),
            Ok(None) => (
                self.module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(ident.clone()))
                    .collect(),
                false,
                false,
            ),
            // A raw lookup failure here isn't this helper's to report --
            // the ordinary path the caller falls back to re-derives (and
            // reports) the identical failure for real.
            Err(_) => return None,
        };
        let (name, module_path) = absolute.split_last()?;
        let raw_candidates = self
            .resolver
            .function_overload_signatures(module_path, name)
            .ok()
            .flatten()?;
        let candidates = if is_alias && !import_reveal {
            raw_candidates
                .into_iter()
                .filter(|(_, _, visibility)| {
                    Self::visibility_allows(*visibility, module_path, &self.module_path)
                })
                .collect()
        } else {
            raw_candidates
        };
        Some((absolute, candidates))
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
    pub(super) fn resolve_overloaded_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };

        if path.is_unqualified() && self.context.find_variable(&path.head).is_some() {
            return Intercepted::Declined;
        }

        // Unqualified (possibly aliased) and module-qualified names take
        // genuinely different paths from here: an alias's candidate set is
        // fixed (and visibility-filtered) at resolution time -- see
        // `resolve_bare_overload_candidates`'s doc comment -- while a
        // module-qualified reference has no alias to fix anything through,
        // so it keeps working exactly as before (every candidate considered,
        // `reveal` at this call site free to bypass the winner's own
        // visibility).
        let (name, module_path, candidates, needs_visibility_check): (Ident, Vec<Ident>, _, bool) =
            if path.is_unqualified() {
                let Some((absolute, candidates)) =
                    self.resolve_bare_overload_candidates(&path.head)
                else {
                    return Intercepted::Declined;
                };
                let Some((name, module_path)) = Self::split_item_path(&absolute) else {
                    return Intercepted::Declined;
                };
                (name, module_path, candidates, false)
            } else {
                let absolute: Vec<Ident> = match self.resolve_alias(&path.head).ok().flatten() {
                    Some(ImportTarget::Module(target)) => target
                        .into_iter()
                        .chain(path.tail.iter().cloned())
                        .collect(),
                    _ => return Intercepted::Declined,
                };
                let Some((name, module_path)) = Self::split_item_path(&absolute) else {
                    return Intercepted::Declined;
                };
                let candidates = match self
                    .resolver
                    .function_overload_signatures(&module_path, &name)
                {
                    Ok(Some(candidates)) => candidates,
                    Ok(None) => return Intercepted::Declined,
                    Err(e) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                        return Intercepted::Claimed(None);
                    }
                };
                (name, module_path, candidates, true)
            };

        let signatures: Vec<(HirId, ResolvedFunctionType)> = candidates
            .iter()
            .map(|(id, fn_type, _)| (*id, fn_type.clone()))
            .collect();

        let Some((winner, args)) =
            self.resolve_overload(node_id, span, &name, &signatures, &call.args)
        else {
            return Intercepted::Claimed(None);
        };
        let (decl_id, fn_type, visibility) = candidates[winner].clone();

        // Only the module-qualified branch needs this: the unqualified/
        // aliased branch already committed to its final candidate set
        // up front (filtered, or fully admitted by `import reveal`), so
        // every possible winner from it is already known-allowed -- a
        // second check here would either be a redundant no-op or, worse,
        // could wrongly *deny* an `import reveal`-admitted hidden winner
        // when the call site itself doesn't also write `reveal` (no
        // ambient `reveal_stack` frame would be active to fall back on).
        if needs_visibility_check && !self.check_visibility(visibility, &module_path) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                    module: module_path.clone(),
                    item: name.clone(),
                }),
            );
            return Intercepted::Claimed(None);
        }

        Intercepted::Claimed(Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            Storage::Function,
            fn_type,
            args,
        )))
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
            fixed.push(if Self::adaptable_literal(arg) {
                None
            } else {
                Some(self.analyze_expr(arg, None)?)
            });
        }

        let mut viable: Vec<(usize, u32)> = Vec::new();
        for (i, (_, fn_type)) in candidates.iter().enumerate() {
            if fn_type.is_variadic || fn_type.params.len() != args.len() {
                continue;
            }
            let mut score = 0u32;
            let mut ok = true;
            for ((_, param_type), (arg, fixed_arg)) in
                fn_type.params.iter().zip(args.iter().zip(&fixed))
            {
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
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoMatchingOverload {
                    name: name.clone(),
                    candidates: candidates.iter().map(|(_, t)| t.clone()).collect(),
                },
            );
            return None;
        };
        let winners: Vec<usize> = viable
            .iter()
            .filter(|&&(_, s)| s == min_score)
            .map(|&(i, _)| i)
            .collect();
        let winner = match winners.as_slice() {
            [only] => *only,
            _ => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::AmbiguousOverload {
                        name: name.clone(),
                        candidates: winners.iter().map(|&i| candidates[i].1.clone()).collect(),
                    },
                );
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
        let default = if n.fractional_part.is_some() {
            ResolvedType::F64
        } else {
            ResolvedType::I32
        };
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
    pub(super) fn resolve_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };

        if path.is_unqualified() && self.context.find_variable(&path.head).is_some() {
            return Intercepted::Declined;
        }

        let absolute: Vec<Ident> = if path.is_unqualified() {
            match self.resolve_alias(&path.head).ok().flatten() {
                Some(ImportTarget::GenericItem(absolute)) => absolute,
                _ => self
                    .module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(path.head.clone()))
                    .collect(),
            }
        } else {
            match self.resolve_alias(&path.head).ok().flatten() {
                Some(ImportTarget::Module(target)) => target
                    .into_iter()
                    .chain(path.tail.iter().cloned())
                    .collect(),
                _ => return Intercepted::Declined,
            }
        };

        let sig: GenericSignature = match self.resolver.generic_function_signature(&absolute) {
            Ok(Some(sig)) => sig,
            Ok(None) => return Intercepted::Declined,
            Err(_) => return Intercepted::Declined,
        };

        Intercepted::Claimed(self.finish_generic_call(node_id, span, call, &absolute, &sig))
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
        let (checked_args, subst) =
            self.infer_generic_args(&sig.generics, &sig.defaults, &sig.params, &call.args)?;

        let type_args = match resolve_inferred_type_args(&sig.generics, &sig.defaults, &subst) {
            Ok(type_args) => type_args,
            Err(generic) => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UnresolvedGenericParam(generic),
                );
                return None;
            }
        };

        let (fn_type, storage, decl_id) =
            match self.resolve_item_checked(absolute, &type_args, true) {
                Ok(ResolvedItem::Value {
                    r#type: ResolvedType::Function(fn_type),
                    storage,
                    decl_id,
                    mutable: _,
                }) => (fn_type, storage, decl_id),
                Ok(_) => {
                    self.error(node_id, span, AnalysisErrorKind::UnresolvedCallee);
                    return None;
                }
                Err(e) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                    return None;
                }
            };

        if checked_args.len() != fn_type.params.len() && !fn_type.is_variadic {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: fn_type.params.len(),
                    found: checked_args.len(),
                },
            );
            return None;
        }
        for (arg, (_, expected_type)) in checked_args.iter().zip(&fn_type.params) {
            if !expected_type.accepts(&arg.r#type) {
                self.error(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: arg.r#type.clone(),
                    },
                );
                return None;
            }
        }

        Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            storage,
            fn_type,
            checked_args,
        ))
    }
}
