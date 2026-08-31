use super::*;

impl<'r> Analyzer<'r> {
    pub(crate) fn resolve_spec_qualified_call(
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
            return self
                .resolve_fully_qualified_spec_call(node_id, span, call, expr_path, qualified);
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
            anchor: path.anchor,
            head: spec_segments[0].clone(),
            tail: spec_segments[1..].to_vec(),
            origin: path.origin,
        };
        let absolute = match self.context.resolve_absolute_item_path(
            &mut *self.resolver,
            &spec_path,
            &self.module_path,
        ) {
            Ok(access) => access.absolute,
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
            let (params, owner) = {
                let cell = spec.borrow();
                let mut owner = cell.module_path.clone();
                owner.push(cell.name.clone());
                (cell.generics.clone(), owner)
            };
            let Some(args) = self.resolve_generic_arg_list(
                node_id,
                span,
                &expr_path.generic_args,
                &owner,
                &params,
            ) else {
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
        let RequirementSignature::Concrete { fn_type, .. } = &declared.signature else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::GenericFunctionNotInstantiated {
                    owner: spec.borrow().name.clone(),
                    function: method_name.clone(),
                    namespace: FunctionNamespace::Member,
                },
            );
            return Intercepted::Claimed(None);
        };
        if fn_type.self_mode.is_none() {
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
        let Some(receiver) = self.analyze_receiver_operand(first) else {
            return Intercepted::Claimed(None);
        };
        let target = receiver.r#type.autoderef().clone();
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
            let adapted_first = match self.adapt_self_argument(
                &call.callee,
                receiver.clone(),
                method.fn_type.self_mode.expect("filtered above"),
            ) {
                Some(adapted) => adapted,
                None => return Intercepted::Claimed(None),
            };
            for (index, (arg, expected)) in call
                .args
                .iter()
                .zip(method.fn_type.param_types())
                .enumerate()
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

        let Some((spec, spec_args)) = self.resolve_spec_reference(node_id, span, &qualified.spec)
        else {
            return Intercepted::Claimed(None);
        };
        let Some(target) = self.resolve_type_or_error(node_id, span, &qualified.target, true)
        else {
            return Intercepted::Claimed(None);
        };

        let Some(flattened) =
            self.flatten_spec(node_id, span, &spec, &spec_args, &ResolvedType::Void)
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
        let RequirementSignature::Concrete { fn_type, .. } = &declared.signature else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::GenericFunctionNotInstantiated {
                    owner: spec.borrow().name.clone(),
                    function: method_name.clone(),
                    namespace: FunctionNamespace::Member,
                },
            );
            return Intercepted::Claimed(None);
        };
        if fn_type.self_mode.is_none() {
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
        spec_args: &[ResolvedGenericArg],
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
        spec_args: &[ResolvedGenericArg],
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
            for (arg, expected) in call.args.iter().zip(method.fn_type.param_types()) {
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
        spec_args: &[ResolvedGenericArg],
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
        let Some(receiver) = self.analyze_receiver_operand(first) else {
            return Intercepted::Claimed(None);
        };
        let target = match target_override {
            Some(target) => target.clone(),
            None => receiver.r#type.autoderef().clone(),
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
            let adapted_first = match self.adapt_self_argument(
                &call.callee,
                receiver.clone(),
                method.fn_type.self_mode.expect("filtered above"),
            ) {
                Some(adapted) => adapted,
                None => return Intercepted::Claimed(None),
            };
            for (index, (arg, expected)) in call
                .args
                .iter()
                .zip(method.fn_type.param_types())
                .enumerate()
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
}
