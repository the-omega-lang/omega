use super::*;

/// One call-shape interceptor's answer. `Declined` means "not my shape, try
/// the next one" -- distinct from claiming the call and failing, which must
/// *not* fall through to another interpretation (its own error was already
/// reported).
pub(super) enum Intercepted {
    Declined,
    Claimed(Option<CheckedExprNode>),
}

/// One call-shape interceptor: peeks at a call and either claims it or
/// declines. A function pointer so interceptors can be listed in priority
/// order at the call site.
pub(super) type Interceptor<'r> = fn(
    &mut Analyzer<'r>,
    HirId,
    Span,
    &HirFunctionCall,
    Option<&ResolvedType>,
) -> Intercepted;

/// A member call's receiver (`base` in `base.method(args)`), resolved once:
/// the place it came from (needed to check writability and to de-assume a
/// narrowing for a `mut self` call), the checked place itself, what it
/// resolves to, and whether it is writable.
struct Receiver {
    place: HirPlace,
    checked: CheckedPlace,
    r#type: ResolvedType,
    mutable: bool,
}

/// A function-call's callee, resolved to either an ordinary value or a bound
/// method reference (a "thiscall"): `base.method(args)` becomes an ordinary
/// call to the method with `&base` (or `base` itself, if already a pointer)
/// prepended as the first (`self`) argument. `HirFunctionDef`'s synthetic
/// `self` parameter already accounts for this in `fn_type`.
pub(super) struct ResolvedCallee {
    pub(super) callee: CheckedExprNode,
    pub(super) fn_type: ResolvedFunctionType,
    pub(super) implicit_self: Option<CheckedExprNode>,
    /// `Some` only when `method` named 2+ overloaded candidates -- overload
    /// resolution already analyzed every argument to score candidates, so
    /// they're handed back here instead of making the ordinary argument
    /// loop redo (and potentially re-error on) the same work.
    pub(super) checked_args: Option<Vec<CheckedExprNode>>,
}

/// `resolve_callee`'s real result: either an ordinary callee or a
/// fully-resolved dynamic-dispatch call (`base.method(...)` where `base` is
/// a `spec *Spec` value), built inline since a dynamic call has no ordinary
/// "callee expression" to hand back. Not a separate interceptor because
/// telling the two apart needs the base place's resolved type, which needs
/// an `analyze_place` call -- a second one on the same base would risk
/// double-reporting a broken base's own errors.
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
    /// a shape the interceptors below can claim: a method call, a call
    /// through a computed expression, or a callee with explicit generic
    /// arguments (already pinned, nothing left to deduce).
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

    /// The checked node for a call to an already-decided function. Shared by
    /// every path that resolves a callee by name rather than by expression
    /// (overloads, static overloads, generic instantiation).
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
                    r#type: function.clone(),
                },
                projections: vec![],
                r#type: function,
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
    /// conforming instance method outside a bound context. The first argument
    /// selects the target conformance; the resulting call is an ordinary
    /// direct call to that conformance's symbol.
    pub(super) fn resolve_spec_qualified_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
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
        if let Some(qualified) = &expr_path.qualified_spec {
            return self.resolve_fully_qualified_spec_call(
                node_id, span, call, expr_path, qualified,
            );
        }
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
            origin: path.origin,
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
        // Looked up before any receiver is demanded, since a receiverless
        // spec function (`Bounded::min()`) has no first argument to look at.
        // Resolved with a placeholder `Self` purely to read `self_mode`; the
        // conformance's own method below is resolved with the real target.
        let Some(flattened) =
            self.flatten_spec(node_id, span, &spec, &spec_args, &ResolvedType::Void)
        else {
            return Intercepted::Claimed(None);
        };
        let Some(declared) = flattened.iter().find(|f| &f.name == method_name) else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec.borrow().name.clone(),
                    function: method_name.clone(),
                },
            );
            return Intercepted::Claimed(None);
        };
        if declared.fn_type.self_mode.is_none() {
            // `Self` can only come from the expected type (`x : char =
            // Bounded::min();`).
            return self.resolve_static_spec_call(
                node_id,
                span,
                call,
                &spec,
                &spec_args,
                method_name,
                declared,
                expected,
            );
        }
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
        // Resolved as a place, once, before any conformance is selected --
        // `conformance_for` needs its type, `adapt_self_argument` needs the
        // place itself. A non-place argument is wrapped in
        // `HirPlaceRoot::Expr` (same shape `synthesize_method_call` uses),
        // so `Display::fmt(42, w)` auto-refs like `(42).fmt(w)`.
        let receiver_place = match &Self::strip_reveal(first).1.expr {
            HirExpr::Place(place) => place.clone(),
            _ => HirPlace {
                root: HirPlaceRoot::Expr(Box::new(first.clone())),
                projections: vec![],
            },
        };
        let Some((checked_receiver, receiver_type, receiver_mutable)) =
            self.analyze_place(first.id, first.span, &receiver_place, None)
        else {
            return Intercepted::Claimed(None);
        };
        let target = receiver_type.autoderef().clone();
        let conformance_methods = match self.resolver.conformance_for(&target, &spec, &spec_args) {
            Ok(Some(conform)) => conform.methods,
            Ok(None)
                if self
                    .type_implements_spec(node_id, span, &target, &spec, &spec_args, false)
                    .is_ok() =>
            {
                match self.resolver.conformances_for_type(&target) {
                    Ok(conformances) => conformances
                        .into_iter()
                        .flat_map(|conform| conform.methods)
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
        let candidates: Vec<_> = conformance_methods
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
            let receiver = Receiver {
                place: receiver_place.clone(),
                checked: checked_receiver.clone(),
                r#type: receiver_type.clone(),
                mutable: receiver_mutable,
            };
            let adapted_first = match self.adapt_self_argument(
                &call.callee,
                receiver,
                method.fn_type.self_mode.expect("filtered above"),
            ) {
                Some(adapted) => adapted,
                None => return Intercepted::Claimed(None),
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

    /// `<Type : Spec>::function(...)` -- the third rung of the `S::fn()` /
    /// `P::fn()` / `<S : P>::fn()` ladder. Both halves of the function's
    /// identity are written in the path, so nothing is inferred. Works for
    /// both static and instance spec functions -- whether the first
    /// argument is a receiver is decided by the resolved function's own
    /// `self` mode, not by the call's shape, which is what lets `<S :
    /// P>::make()` resolve a case `S::make()` diagnoses as ambiguous and
    /// `P::make()` misparses as a receiver-taking call.
    fn resolve_fully_qualified_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expr_path: &ExprPath,
        qualified: &QualifiedSpecPath,
    ) -> Intercepted {
        // The parser guarantees exactly one segment, no expression-level
        // generic args (the spec's own type arguments live in `qualified.spec`).
        debug_assert!(expr_path.path.tail.is_empty() && expr_path.generic_args.is_empty());
        let method_name = expr_path.path.head.clone();

        let Some((spec, spec_args)) =
            self.resolve_spec_reference(node_id, span, &qualified.spec)
        else {
            // Failure already reported (NotASpec / unresolved type).
            return Intercepted::Claimed(None);
        };
        let Some(target) = self.resolve_type_or_error(node_id, span, &qualified.target, true)
        else {
            return Intercepted::Claimed(None);
        };

        let Some(flattened) = self.flatten_spec(node_id, span, &spec, &spec_args, &ResolvedType::Void)
        else {
            return Intercepted::Claimed(None);
        };
        let Some(declared) = flattened.iter().find(|f| &f.name == &method_name) else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec.borrow().name.clone(),
                    function: method_name.clone(),
                },
            );
            return Intercepted::Claimed(None);
        };
        if declared.fn_type.self_mode.is_none() {
            return self.resolve_static_spec_call(
                node_id,
                span,
                call,
                &spec,
                &spec_args,
                &method_name,
                declared,
                Some(&target),
            );
        }
        self.resolve_instance_spec_call(
            node_id,
            span,
            call,
            &spec,
            &spec_args,
            &method_name,
            Some(&target),
        )
    }

    /// The conformance methods for one `(target, spec, spec_args)` -- shared
    /// by every spec-qualified call shape: a direct entry, or (for a spec
    /// alias, never itself conformed to) the members' own entries, with
    /// `SpecNotImplemented` when neither exists. `None` means the failure
    /// was already reported.
    fn spec_call_conformance_methods(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Option<Vec<(Ident, ResolvedMethod)>> {
        match self.resolver.conformance_for(target, spec, spec_args) {
            Ok(Some(conform)) => Some(conform.methods),
            Ok(None)
                if self
                    .type_implements_spec(node_id, span, target, spec, spec_args, false)
                    .is_ok() =>
            {
                match self.resolver.conformances_for_type(target) {
                    Ok(conformances) => Some(
                        conformances
                            .into_iter()
                            .flat_map(|conform| conform.methods)
                            .collect(),
                    ),
                    Err(err) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                        None
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
                None
            }
            Err(err) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                None
            }
        }
    }

    /// The receiverless half of a spec-qualified call -- `Spec::static_fn()`
    /// or `<S : Spec>::static_fn(...)`. On the bare spelling the target can
    /// only come from the expected type, and only when the declared return
    /// type is exactly `Self` (anything else never names the implementing
    /// type). Both failures get a dedicated diagnostic, never a bogus
    /// argument count.
    fn resolve_static_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
        method_name: &Ident,
        declared: &FlattenedSpecFn,
        target: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(target) = target else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::SpecStaticNeedsExpectedType {
                    spec: spec.borrow().name.clone(),
                    function: method_name.clone(),
                },
            );
            return Intercepted::Claimed(None);
        };
        let returns_self = matches!(
            &declared.raw.return_type,
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "Self"
        );
        if !returns_self {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::SpecStaticReturnNotSelf {
                    spec: spec.borrow().name.clone(),
                    function: method_name.clone(),
                    return_type: crate::error::raw_type_display(&declared.raw.return_type),
                },
            );
            return Intercepted::Claimed(None);
        }
        let Some(conformance_methods) =
            self.spec_call_conformance_methods(node_id, span, target, spec, spec_args)
        else {
            return Intercepted::Claimed(None);
        };
        let candidates: Vec<_> = conformance_methods
            .into_iter()
            .filter(|(name, method)| name == method_name && method.fn_type.self_mode.is_none())
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
            for (arg, (_, expected)) in call.args.iter().zip(&method.fn_type.params) {
                let Some(checked) = self.analyze_expr(arg, Some(expected)) else {
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

    /// The receiver-taking half of a spec-qualified call, shared by
    /// `Spec::method(recv, ...)` (target read off the receiver) and `<S :
    /// Spec>::method(recv, ...)` (target written explicitly). The receiver
    /// is resolved as a place, once (see `resolve_spec_qualified_call`'s
    /// identical handling for why).
    fn resolve_instance_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
        method_name: &Ident,
        target_override: Option<&ResolvedType>,
    ) -> Intercepted {
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
        let receiver_place = match &Self::strip_reveal(first).1.expr {
            HirExpr::Place(place) => place.clone(),
            _ => HirPlace {
                root: HirPlaceRoot::Expr(Box::new(first.clone())),
                projections: vec![],
            },
        };
        let Some((checked_receiver, receiver_type, receiver_mutable)) =
            self.analyze_place(first.id, first.span, &receiver_place, None)
        else {
            return Intercepted::Claimed(None);
        };
        let target = match target_override {
            Some(target) => target.clone(),
            None => receiver_type.autoderef().clone(),
        };
        let Some(conformance_methods) =
            self.spec_call_conformance_methods(node_id, span, &target, spec, spec_args)
        else {
            return Intercepted::Claimed(None);
        };
        let candidates: Vec<_> = conformance_methods
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
            let receiver = Receiver {
                place: receiver_place.clone(),
                checked: checked_receiver.clone(),
                r#type: receiver_type.clone(),
                mutable: receiver_mutable,
            };
            let adapted_first = match self.adapt_self_argument(
                &call.callee,
                receiver,
                method.fn_type.self_mode.expect("filtered above"),
            ) {
                Some(adapted) => adapted,
                None => return Intercepted::Claimed(None),
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

    /// What a call's callee expression resolves to. `base.field(args)` is
    /// the interesting shape: `field` may name a method (implicit-`self`
    /// call), a callable field, or -- when `base` is a `spec *Spec` value --
    /// a dynamically dispatched spec function. Everything else is an
    /// ordinary expression whose type must be a function.
    pub(super) fn resolve_callee(
        &mut self,
        callee: &HirExprNode,
        args: &[HirExprNode],
    ) -> Option<CalleeResolution> {
        // `reveal` is transparent, so every use of `callee` below sees
        // through it; `was_reveal` feeds `with_reveal_bypass` below since
        // there's no enclosing `Reveal` arm to rely on here.
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
        // A method call reads its receiver, so `x` in `x.method()` is used.
        // Marked here rather than inside `analyze_place` (which also serves
        // assignment targets, where marking used would wrongly silence
        // `UnusedVariable` on write-only bindings).
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked.root {
            self.context.mark_used(decl_id);
        }
        let receiver = Receiver {
            place: base_place,
            checked,
            r#type,
            mutable,
        };

        // Dynamic dispatch: the receiver is a `spec *Spec` value, so `field`
        // is looked up in the spec's own flattened function list -- the
        // implementor is erased, so there's no concrete `functions` list.
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
                match self.resolver.conformances_for_type(receiver_type) {
                    Ok(conformances) => {
                        if let Some(conform) = conformances.iter().find(|conform| {
                            conform.methods.iter().any(|(name, method)| {
                                name == field && method.fn_type.self_mode.is_some()
                            })
                        }) {
                            self.error(
                                callee.id,
                                callee.span,
                                AnalysisErrorKind::MethodNotInScope {
                                    method: field.clone(),
                                    spec: conform.spec.borrow().name.clone(),
                                    r#type: receiver_type.clone(),
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

        // Read straight off the resolved signature, never reverse-engineered
        // from `params[0]`'s type shape.
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
                    r#type: fn_type.clone(),
                },
                projections: vec![],
                r#type: fn_type,
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
    /// `self` is static -- reached only through the type's own name, never
    /// an instance -- so only member candidates are callable here.
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
            // Scored without each candidate's synthesized `self` param --
            // `self` is never itself overload-distinguishing. The winner's
            // real (with-`self`) signature is read back by index below.
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

        // A primitive target has no declared name (reachable here only when
        // a self-less `primitive`/`conform` function shares a name with an
        // instance call), so its own `Display` stands in.
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
        // A primitive receiver's methods come from a `primitive` block or a
        // `conform`, neither with a declaring owner, so the check below is
        // trivially true whatever owner is passed.
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

    /// Adapts the receiver to whatever `self_mode` declared: a 2x2 matrix of
    /// (self wants a pointer / a value) x (receiver is a pointer / a value),
    /// plus `Str`/`Slice` receivers, already fat pointers, needing no
    /// wrapper of their own.
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

        // `comp_binding.method(...)` -- resolved separately, since the
        // ordinary arms below all build a `CheckedExpr::Place`/`AddressOf`
        // node, which a `Storage::Comp` place has no codegen meaning for.
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
            // coerced to the pointer shape self expects, rather than a
            // deref-then-address-of round trip to the same value.
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
            // already its own fat-pointer representation, so `AddressOf`
            // would add a genuine extra indirection layer; re-stamped with
            // self_mode's own mutability instead.
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
                    // De-assumption, like an explicit `&mut`.
                    if let Some((ident, origin, ..)) = self.narrowable_place(&place) {
                        self.context.widen_variable(&ident, origin);
                    }
                }
                // Widened like an explicit `&`/`&mut`: self is
                // `*Self`/`*mut Self`, never `*Self::Variant`.
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
            // copy. No mutability check: reading through any pointer is
            // always legal, unlike writing through one.
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let pointee = pointee.widened();
                let mut place = checked;
                place.projections.push(CheckedProjection::Deref {
                    r#type: pointee.clone(),
                });
                Some(node(pointee, CheckedExpr::Place(place)))
            }
            // self wants a value, the receiver already is one -- passed
            // through unchanged; mutating a `mut self` copy inside the
            // method never touches the caller's original.
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Place(checked))),
        }
    }

    /// `adapt_self_argument`'s `Storage::Comp` counterpart -- the same
    /// pointer/value matrix, built from an already-known `ConstValue` (via
    /// `resolve_comp_place`) instead of a real place, since a `comp` binding
    /// has no address of its own. A pointer-wanting `self` needs const
    /// promotion like a bare `&comp_binding` (see `analyze_address_of` and
    /// docs/19-compile-time-evaluation.md); a value-wanting `self` needs
    /// none. `*mut self`/`&mut` is never legal here: the promoted data is
    /// read-only rodata, so no writable pointer can ever come from it.
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
            // reachable via `&<place>` inside an earlier `comp` evaluation
            // (see `ConstValue::Ref`) -- reused as-is, widened.
            (ResolvedType::Pointer { pointee, .. }, true) => {
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(pointee.widened()),
                    mutable: false,
                };
                Some(node(pointer, CheckedExpr::Const(value)))
            }
            // self wants a pointer, receiver is `Str`/`Slice` -- already its
            // own fat-pointer representation, used as-is.
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
            // self wants a pointer, receiver is a plain value -- const
            // promotion (see `analyze_address_of`).
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
            // self wants a value, receiver is a pointer -- auto-deref:
            // unwrap the `ConstValue::Ref` layer `&<place>` produced.
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let ConstValue::Ref(inner) = value else {
                    unreachable!(
                        "a comp value's own type is only ever Pointer alongside a ConstValue::Ref -- see ConstValue::Ref's doc comment"
                    );
                };
                Some(node(pointee.widened(), CheckedExpr::Const(*inner)))
            }
            // self wants a value, receiver already is one -- unchanged.
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
    /// place instead of re-resolving from scratch, to avoid double-reporting
    /// the base's own errors.
    fn resolve_field_callee(
        &mut self,
        callee: &HirExprNode,
        was_reveal: bool,
        field: &Ident,
        receiver: Receiver,
    ) -> Option<CalleeResolution> {
        // Indirect calls through a `comp` binding's field are unsupported
        // (see finding); caught here, not in `resolve_callee`, so a
        // comp-binding *method* call isn't dragged down by the restriction.
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
            r#type: _,
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
            kind: CheckedExpr::Place(CheckedPlace { root, projections, r#type: field_type.clone() }),
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
    /// determined `base`'s type is `spec<type_args> *_` -- `field` is looked
    /// up by *position* in the spec's flattened function list
    /// (`flatten_spec`), the same order `Codegen`'s vtable builder uses, so
    /// the two always agree. `Self` is bound to a placeholder (`Void`)
    /// purely to give the resolved signature the right leaf shape for
    /// codegen -- a pointer's leaf count never depends on what it points to.
    /// Argument checking mirrors the ordinary call loop but is kept
    /// separate since there's no ordinary `callee`/`fn_type` pairing here to
    /// reuse it through.
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
        let matches: Vec<usize> = flattened
            .iter()
            .enumerate()
            .filter(|(_, f)| &f.name == field)
            .map(|(index, _)| index)
            .collect();
        let slot_index = match matches.as_slice() {
            [] => {
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
            }
            [index] => *index,
            // Two of this object's specs declare the same function name --
            // must not silently pick the first slot (static dispatch through
            // a conjunction bound already rejects the identical shape).
            _ => {
                let specs: Vec<Ident> = matches
                    .iter()
                    .map(|&index| flattened[index].spec_name.clone())
                    .collect();
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::AmbiguousSpecObjectMethod {
                        function: field.clone(),
                        specs,
                    },
                );
                return None;
            }
        };
        let fn_type = flattened[slot_index].fn_type.clone();
        // params[0] is the synthesized self, never counted against the
        // user's own arguments -- like an ordinary method call.
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

    /// If `call`'s callee is `Type::function(args)` where `function` names
    /// 2+ overloaded (non-member, static) candidates, resolves the whole
    /// call here via `resolve_overload`. Returns `Declined` for anything
    /// that isn't this exact shape -- most importantly a name with 0 or 1
    /// static candidates, which falls through to `resolve_type_member`'s
    /// unchanged single-candidate path. Deliberately scoped to a *locally
    /// visible* type name (this module's own, or an imported alias); see
    /// finding on the module-qualified gap this leaves.
    pub(super) fn resolve_overloaded_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        _expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };
        let [member] = path.tail.as_slice() else {
            return Intercepted::Declined;
        };

        // A module alias wins over a type interpretation whenever both could
        // apply, so a genuine `module::function` shape is never misread as
        // `Type::function` here. Silent probe -- a real resolution failure
        // isn't this function's to report; left for whichever fallback path
        // needs this same alias to surface it.
        let accessor = self.path_module(path);
        let alias = self.resolver.resolve_import_alias(&accessor, &path.head).ok().flatten();
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

        // Checked after argument-driven resolution picks a winner --
        // `resolve_overload` itself has no notion of visibility, only
        // signature fit.
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
    /// generic struct/union/enum with no explicit `<...>` and `function`
    /// names exactly one non-overloaded, `self`-less (static) function,
    /// infers `Owner`'s omitted type arguments from the call's argument
    /// types -- the same duck-typed unification `resolve_generic_call` gives
    /// a bare generic function, extended across the owner/function boundary.
    /// Declines for an already-concrete `Owner`, a module-qualified callee,
    /// 2+ overloaded static candidates (composing overload scoring with
    /// owner-generic inference is a separable follow-up, not attempted
    /// here), or a static function with independent generics of its own
    /// ("struct generics are always explicit, so by the time a value of
    /// that type exists its methods are already fully monomorphized").
    pub(super) fn resolve_generic_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
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
        // alias check.
        let accessor = self.path_module(path);
        let alias = self.resolver.resolve_import_alias(&accessor, &path.head).ok().flatten();
        let absolute: Vec<Ident> = match &alias {
            Some(ImportTarget::Item(absolute, _)) | Some(ImportTarget::GenericItem(absolute)) => {
                absolute.clone()
            }
            Some(ImportTarget::Module(_)) => return Intercepted::Declined,
            None => accessor
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect(),
        };

        let Some((real_absolute, sig)) = self.generic_static_function_signature_with_ambient(
            &accessor,
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
            &accessor,
            std::slice::from_ref(&path.head),
            &real_absolute,
            member,
            &sig,
            expected,
        ))
    }

    /// `resolver.generic_static_function_signature`, retried against the
    /// `core` ambient fallback when `prefix` is a genuinely unqualified
    /// single segment and the direct lookup finds nothing -- the same retry
    /// `generic_literal_signature_with_ambient` (literals.rs) gives the
    /// literal-construction path. Hands back whichever absolute path
    /// actually matched, since instantiation needs the real declaration's
    /// path, not the own-module guess that found nothing.
    fn generic_static_function_signature_with_ambient(
        &mut self,
        accessor: &[Ident],
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
            .ambient_core_candidates(accessor, single)
            .ok()
            .flatten()?;
        let sig = self
            .resolver
            .generic_static_function_signature(&ambient, function_name)
            .ok()
            .flatten()?;
        Some((ambient, sig))
    }

    /// Builds the inference seed from the call's expected type: unify the
    /// signature's declared return type against `expected` into a fresh map,
    /// widening every seeded entry. Precedence is deliberately **expected
    /// type > argument-driven inference > declared default**, matching
    /// `infer_literal_type_args`'s order. Widening follows
    /// `resolve_inferred_type_args`'s rule: a caller's enum-variant
    /// refinement (`T = MyEnum::Second`) must never mint a spurious
    /// instantiation.
    fn seed_from_expected(
        expected: Option<&ResolvedType>,
        generics: &[Ident],
        return_type: &Type,
    ) -> HashMap<Ident, ResolvedType> {
        let mut seed = HashMap::new();
        if let Some(expected) = expected {
            unify_generic_type(generics, return_type, expected, &mut seed);
            for resolved in seed.values_mut() {
                *resolved = resolved.widened();
            }
        }
        seed
    }

    /// Scans raw parameter types against checked argument types for a
    /// `Type::Pointer(Type::Named(g))` with `g` an unbound generic matched
    /// against a `Slice`/`Str` argument -- the thin-pointer-against-
    /// fat-pointer inference failure, which gets its own teaching
    /// diagnostic rather than the bare "cannot infer" one.
    fn fat_pointer_generic_mismatch(
        generics: &[Ident],
        params: &[Type],
        args: &[CheckedExprNode],
        subst: &HashMap<Ident, ResolvedType>,
    ) -> Option<(Ident, ResolvedType)> {
        for (raw, arg) in params.iter().zip(args) {
            let Type::Pointer(inner, _) = raw else { continue };
            let Type::Named(path) = inner.as_ref() else { continue };
            if !path.is_unqualified()
                || !generics.contains(&path.head)
                || subst.contains_key(&path.head)
            {
                continue;
            }
            if matches!(
                arg.r#type,
                ResolvedType::Slice { .. } | ResolvedType::Str { .. }
            ) {
                return Some((path.head.clone(), arg.r#type.clone()));
            }
        }
        None
    }

    /// The actual work behind `resolve_generic_static_call`, once confirmed
    /// `call`'s callee names a single-candidate static function on a generic
    /// type at `owner_absolute` -- split out so the caller stays a single
    /// check, mirroring `finish_generic_call`'s identical split.
    fn finish_generic_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &[Ident],
        prefix: &[Ident],
        owner_absolute: &[Ident],
        member: &Ident,
        sig: &GenericStaticFunctionSignature,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (checked_args, subst) = self.infer_generic_args(
            &sig.owner_generics,
            &sig.owner_defaults,
            &sig.params,
            &call.args,
            Self::seed_from_expected(expected, &sig.owner_generics, &sig.return_type),
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
                    if let Some((parameter, found)) = Self::fat_pointer_generic_mismatch(
                        &sig.owner_generics,
                        &sig.params,
                        &checked_args,
                        &subst,
                    ) {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::GenericParamFromFatPointer { parameter, found },
                        );
                    } else {
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
                    }
                    return None;
                }
            };

        let owner_type = match self.resolve_item_with_ambient_from(
            accessor,
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

    /// If `ident` -- unqualified, whether declared in this module or reached
    /// through a named import alias -- names an *overloaded* free function
    /// (2+ candidates), returns its real absolute path and candidate list.
    /// For an aliased name, filtered to overloads this module can see,
    /// unless the import itself was written `reveal` (see finding on why a
    /// use-site `reveal` alone no longer suffices here). Own-module names
    /// are never filtered.
    ///
    /// `None` means "not an overloaded name at all" (0 or 1 candidates) --
    /// the caller falls through to the ordinary single-item path.
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
            // A raw lookup failure isn't this helper's to report -- the
            // caller's ordinary fallback path re-derives it for real.
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
    /// it), resolves the whole call here via argument-driven overload
    /// resolution (`resolve_overload`) instead of the ordinary
    /// `resolve_callee`-then-args pipeline. Declines for anything that
    /// isn't this exact shape -- most importantly a name with 0 or 1
    /// candidates, the overwhelming majority of calls, which stays on the
    /// unchanged ordinary path.
    pub(super) fn resolve_overloaded_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        _expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };

        if path.is_unqualified()
            && self.context.find_variable(&path.head, path.origin).is_some()
        {
            return Intercepted::Declined;
        }

        // Unqualified (possibly aliased) and module-qualified names take
        // different paths: an alias's candidate set is fixed and
        // visibility-filtered at resolution time (see
        // `resolve_bare_overload_candidates`), while a module-qualified
        // reference has no alias to fix anything through, so every
        // candidate is considered and `reveal` at the call site can still
        // bypass the winner's visibility.
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

        // Only the module-qualified branch needs this: the aliased branch
        // already committed to its final (filtered or `import reveal`
        // -admitted) candidate set up front, so a second check here could
        // wrongly deny an `import reveal`-admitted winner when the call
        // site itself doesn't also write `reveal`.
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

    /// Resolves a call against 2+ same-named candidates by argument type.
    /// `candidates` pairs each overload's identity (a `HirId`) with its
    /// resolved signature; `args` are the call's raw, not-yet-analyzed
    /// argument expressions.
    ///
    /// Every argument that isn't an `adaptable_literal` is analyzed once, up
    /// front, since its resolved type can't depend on which candidate wins
    /// -- this avoids double-analyzing (and double-erroring on) a
    /// fixed-type argument across every candidate. An adaptable-literal
    /// argument is instead scored per candidate via `literal_overload_fit`,
    /// silently: a candidate is viable iff every argument fits its
    /// parameter, and its score is how many adaptable-literal arguments
    /// needed a type other than their natural default (`i32`/`f32`) to fit
    /// (0 means every literal stayed at its default). The unique
    /// minimum-score viable candidate wins; zero viable is
    /// `NoMatchingOverload`, a tie at the minimum is `AmbiguousOverload`.
    /// The winner's own adaptable-literal arguments are then analyzed for
    /// real -- the only point they're committed to a concrete type.
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
                    None => match Self::literal_overload_fit(arg, param_type, self.target.pointer_bits()) {
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

    /// Whether an `adaptable_literal` argument fits `target` for overload
    /// viability, and -- if so -- whether `target` is exactly the literal's
    /// natural default type (`i32`/`f32`); see `resolve_overload` for how
    /// the result is used. `None` if it doesn't fit at all. Deliberately
    /// silent -- a rejected candidate might not be the one the call
    /// ultimately resolves to.
    fn literal_overload_fit(arg: &HirExprNode, target: &ResolvedType, pointer_bits: u32) -> Option<bool> {
        let n = match &arg.expr {
            HirExpr::Number(n) => n,
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => n,
                _ => return None,
            },
            _ => return None,
        };
        let target_kind = target.numeric_kind(pointer_bits)?;
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
    /// `resolve_callee`-then-args pipeline. `Claimed` either way (even on a
    /// reported error) since the caller must not fall through and
    /// double-report. Declines, untouched, for anything that isn't this
    /// shape: a method-call callee (struct generics are always explicit, so
    /// by the time a value exists its methods are already monomorphized), a
    /// callee that isn't a zero-projection path, a path shadowed by a local
    /// binding (never generic), a qualified path whose head isn't a
    /// recognized import alias, or a name that isn't a generic function at
    /// all.
    pub(super) fn resolve_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };

        if path.is_unqualified()
            && self.context.find_variable(&path.head, path.origin).is_some()
        {
            return Intercepted::Declined;
        }

        let accessor = self.path_module(path);
        let absolute: Vec<Ident> = if path.is_unqualified() {
            match self.resolver.resolve_import_alias(&accessor, &path.head).ok().flatten() {
                Some(ImportTarget::GenericItem(absolute)) => absolute,
                _ => accessor
                    .iter()
                    .cloned()
                    .chain(std::iter::once(path.head.clone()))
                    .collect(),
            }
        } else {
            match self.resolver.resolve_import_alias(&accessor, &path.head).ok().flatten() {
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

        Intercepted::Claimed(self.finish_generic_call(
            node_id, span, call, &accessor, &absolute, &sig, expected,
        ))
    }

    /// The actual work behind `resolve_generic_call`, once confirmed
    /// `call`'s callee names a generic function at `absolute` -- split out
    /// so the caller can stay a single "does this even apply" check.
    fn finish_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &[Ident],
        absolute: &[Ident],
        sig: &GenericSignature,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (checked_args, subst) = self.infer_generic_args(
            &sig.generics,
            &sig.defaults,
            &sig.params,
            &call.args,
            Self::seed_from_expected(expected, &sig.generics, &sig.return_type),
        )?;

        let type_args = match resolve_inferred_type_args(&sig.generics, &sig.defaults, &subst) {
            Ok(type_args) => type_args,
            Err(generic) => {
                if let Some((parameter, found)) = Self::fat_pointer_generic_mismatch(
                    &sig.generics,
                    &sig.params,
                    &checked_args,
                    &subst,
                ) {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::GenericParamFromFatPointer { parameter, found },
                    );
                } else {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedGenericParam(generic),
                    );
                }
                return None;
            }
        };

        let (fn_type, storage, decl_id) =
            match self.resolver.resolve_item(accessor, absolute, &type_args, true, false) {
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
