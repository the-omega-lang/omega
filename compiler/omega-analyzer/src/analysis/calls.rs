use super::*;

pub(super) enum Intercepted {
    Declined,
    Claimed(Option<CheckedExprNode>),
}

pub(super) type Interceptor<'r> = fn(
    &mut Analyzer<'r>,
    HirId,
    Span,
    &HirFunctionCall,
    Option<&ResolvedType>,
) -> Intercepted;

struct Receiver {
    place: HirPlace,
    checked: CheckedPlace,
    r#type: ResolvedType,
    mutable: bool,
}

pub(super) struct ResolvedCallee {
    pub(super) callee: CheckedExprNode,
    pub(super) fn_type: ResolvedFunctionType,
    pub(super) implicit_self: Option<CheckedExprNode>,
    pub(super) checked_args: Option<Vec<CheckedExprNode>>,
}

pub(super) enum CalleeResolution {
    Ordinary(ResolvedCallee),
    Dynamic(Option<CheckedExprNode>),
}

impl<'r> Analyzer<'r> {
    fn split_item_path(absolute: &[Ident]) -> Option<(Ident, Vec<Ident>)> {
        absolute
            .split_last()
            .map(|(name, module)| (name.clone(), module.to_vec()))
    }

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

    fn resolve_fully_qualified_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expr_path: &ExprPath,
        qualified: &QualifiedSpecPath,
    ) -> Intercepted {
        debug_assert!(expr_path.path.tail.is_empty() && expr_path.generic_args.is_empty());
        let method_name = expr_path.path.head.clone();

        let Some((spec, spec_args)) =
            self.resolve_spec_reference(node_id, span, &qualified.spec)
        else {
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

    fn require_method_visible(
        &mut self,
        callee: &HirExprNode,
        was_reveal: bool,
        field: &Ident,
        receiver_type: &ResolvedType,
        method: &ResolvedMethod,
    ) -> Option<()> {
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
            (_, true) => {
                if wants_mutable {
                    self.require_mutable_place(id, span, &place.root, &checked, mutable)?;
                    // De-assumption, like an explicit `&mut`.
                    if let Some((ident, origin, ..)) = self.narrowable_place(&place) {
                        self.context.widen_variable(&ident, origin);
                    }
                }
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(r#type.widened()),
                    mutable: wants_mutable,
                };
                Some(node(
                    pointer,
                    CheckedExpr::AddressOf(CheckedAddressOf { place: checked }),
                ))
            }
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let pointee = pointee.widened();
                let mut place = checked;
                place.projections.push(CheckedProjection::Deref {
                    r#type: pointee.clone(),
                });
                Some(node(pointee, CheckedExpr::Place(place)))
            }
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Place(checked))),
        }
    }

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
            (ResolvedType::Pointer { pointee, .. }, true) => {
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(pointee.widened()),
                    mutable: false,
                };
                Some(node(pointer, CheckedExpr::Const(value)))
            }
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
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let ConstValue::Ref(inner) = value else {
                    unreachable!(
                        "a comp value's own type is only ever Pointer alongside a ConstValue::Ref -- see ConstValue::Ref's doc comment"
                    );
                };
                Some(node(pointee.widened(), CheckedExpr::Const(*inner)))
            }
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Const(value))),
        }
    }

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

    fn resolve_field_callee(
        &mut self,
        callee: &HirExprNode,
        was_reveal: bool,
        field: &Ident,
        receiver: Receiver,
    ) -> Option<CalleeResolution> {
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
