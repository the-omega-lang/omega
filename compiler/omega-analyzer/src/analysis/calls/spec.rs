use super::*;

/// A spec with its arguments by position; `None` is a hole the target's
/// conformances determine.
type OpenSpecApplication = (NominalRef, Vec<Option<ResolvedGenericArg>>);

/// Each function a conformance provides, with the spec application that
/// provides it, as diagnostics name it.
type ConformingMethods = Vec<(Ident, ResolvedMethod, String)>;

fn conforming_methods(
    conformances: Vec<crate::resolved_type::ResolvedConformance>,
) -> ConformingMethods {
    let applications: Vec<_> = conformances
        .iter()
        .map(|conform| {
            crate::resolved_type::ResolvedSpecApplication::new(
                conform.spec.clone(),
                conform.spec_args.clone(),
            )
        })
        .collect();
    conformances
        .into_iter()
        .zip(spec_displays(&applications))
        .flat_map(|(conform, provider)| {
            conform
                .methods
                .into_iter()
                .map(move |(name, method)| (name, method, provider.clone()))
        })
        .collect()
}

impl<'r> Analyzer<'r> {
    pub(crate) fn resolve_spec_qualified_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Expected<'_>,
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
                node_id, span, call, expr_path, qualified, expected,
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
        let spec_at = spec_segments.len() - 1;
        if !expr_path.generic_args.only_on(&[spec_at]) {
            return Intercepted::Declined;
        }
        let spec_path = Path {
            anchor: path.anchor,
            head: spec_segments[0].clone(),
            tail: spec_segments[1..].to_vec(),
            origin: path.origin,
        };
        let Some(spec) = self.spec_declaration_for_path(&spec_path) else {
            return Intercepted::Declined;
        };
        let spec_args = match expr_path.generic_args.args_on(spec_at) {
            Some(written) => {
                let Some(written) = self.ordinary_generic_args(node_id, span, written, "a spec")
                else {
                    return Intercepted::Claimed(None);
                };
                match self.written_spec_args(node_id, span, &spec, &written) {
                    Some(spec_args) => spec_args,
                    None => return Intercepted::Claimed(None),
                }
            }
            None => vec![None; self.resolver.types().spec_facts(&spec).generics.len()],
        };
        self.resolve_spec_call(
            node_id,
            span,
            call,
            &spec,
            &spec_args,
            method_name,
            None,
            expected,
            path.origin,
        )
    }

    /// The spec a qualifying path names, if it names one. Silent: a path
    /// that is not a spec is some other call shape.
    fn spec_declaration_for_path(&mut self, spec_path: &Path) -> Option<NominalRef> {
        let absolute = self
            .context
            .resolve_absolute_item_path(&mut *self.resolver, spec_path, &self.module_path)
            .ok()?
            .absolute;
        match self.resolver.spec_declaration(&absolute) {
            Ok(Some(spec)) => Some(spec),
            _ if spec_path.is_unqualified() => {
                let ambient = self
                    .resolver
                    .ambient_core_candidates(&self.module_path, &spec_path.head)
                    .ok()
                    .flatten()?;
                self.resolver.spec_declaration(&ambient).ok().flatten()
            }
            _ => None,
        }
    }

    /// A written spec argument list, positionally: a `_` is a hole the
    /// target's conformances determine.
    fn written_spec_args(
        &mut self,
        node_id: HirId,
        span: Span,
        spec: &NominalRef,
        written: &[GenericArg],
    ) -> Option<Vec<Option<ResolvedGenericArg>>> {
        let params = self.resolver.types().spec_facts(spec).generics.clone();
        let owner = spec.module_path.join(spec.name.clone());
        self.check_generic_arity(node_id, span, &owner, &params, written.len())?;
        let mut args = Vec::with_capacity(written.len());
        let mut ok = true;
        for (index, arg) in written.iter().enumerate() {
            if *arg == GenericArg::Infer {
                args.push(None);
                continue;
            }
            match self.resolve_generic_arg_or_error(node_id, span, arg, params.get(index)) {
                Some(resolved) => args.push(Some(resolved)),
                None => ok = false,
            }
        }
        ok.then_some(args)
    }

    /// `P::f(...)`, `P<A, _>::f(...)`, `<S : P>::f(...)`: the requirement's
    /// shape comes from the spec's own declaration, and the target -- the
    /// written one, the receiver, or the expected type -- supplies the
    /// conformance whose function is called.
    #[allow(clippy::too_many_arguments)]
    fn resolve_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        spec: &NominalRef,
        spec_args: &[Option<ResolvedGenericArg>],
        method_name: &Ident,
        target: Option<&ResolvedType>,
        expected: Expected<'_>,
        origin: Origin,
    ) -> Intercepted {
        let requirement = self
            .resolver
            .types()
            .spec_facts(spec)
            .functions
            .iter()
            .find(|(name, _)| name == method_name)
            .map(|(_, raw)| raw.clone());
        let Some(requirement) = requirement else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec.name.clone(),
                    function: method_name.clone(),
                },
            );
            return Intercepted::Claimed(None);
        };
        if !self.require_requirement_visible(node_id, span, spec, &requirement, origin) {
            return Intercepted::Claimed(None);
        }
        if !requirement.generics.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::GenericFunctionNotInstantiated {
                    owner: spec.name.clone(),
                    function: method_name.clone(),
                    namespace: FunctionNamespace::Member,
                },
            );
            return Intercepted::Claimed(None);
        }
        if requirement.self_mode.is_none() {
            return self.resolve_static_spec_call(
                node_id,
                span,
                call,
                spec,
                spec_args,
                method_name,
                &requirement.return_type,
                target.or(expected.exact()),
            );
        }
        self.resolve_instance_spec_call(node_id, span, call, spec, spec_args, method_name, target)
    }

    fn resolve_fully_qualified_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expr_path: &ExprPath,
        qualified: &QualifiedSpecPath,
        expected: Expected<'_>,
    ) -> Intercepted {
        debug_assert!(expr_path.path.tail.is_empty() && expr_path.generic_args.is_empty());
        let method_name = expr_path.path.head.clone();

        // `<_ : P>::f` is `P::f`, whose `Self` comes from the receiver or the
        // expected type; `<S : _>::f` names the one conformed spec of `S`
        // that declares `f`. Nothing infers both halves at once.
        let (target, (spec, spec_args)) = match (&qualified.target, &qualified.spec) {
            (Type::Infer, Type::Infer) => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::InferenceHoleNotAllowed),
                );
                return Intercepted::Claimed(None);
            }
            (target, Type::Infer) => {
                let Some(target) = self.resolve_type_or_error(node_id, span, target, true) else {
                    return Intercepted::Claimed(None);
                };
                let Some((spec, spec_args)) =
                    self.infer_qualified_spec(node_id, span, &target, &method_name)
                else {
                    return Intercepted::Claimed(None);
                };
                (
                    Some(target),
                    (spec, spec_args.into_iter().map(Some).collect()),
                )
            }
            // Nothing infers the target and the spec's written arguments
            // together.
            (Type::Infer, Type::Generic(_, written)) if written.contains(&GenericArg::Infer) => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::InferenceHoleNotAllowed),
                );
                return Intercepted::Claimed(None);
            }
            (target, spec) => {
                let Some(spec) = self.qualifying_spec_application(node_id, span, spec) else {
                    return Intercepted::Claimed(None);
                };
                let target = match target {
                    Type::Infer => None,
                    target => match self.resolve_type_or_error(node_id, span, target, true) {
                        Some(target) => Some(target),
                        None => return Intercepted::Claimed(None),
                    },
                };
                (target, spec)
            }
        };

        self.resolve_spec_call(
            node_id,
            span,
            call,
            &spec,
            &spec_args,
            &method_name,
            target.as_ref(),
            expected,
            expr_path.path.origin,
        )
    }

    /// The spec half of `<S : P<...>>`. An omitted list, or a `_` among its
    /// arguments, leaves holes the target's conformances determine; anything
    /// else is an ordinary spec reference.
    fn qualifying_spec_application(
        &mut self,
        node_id: HirId,
        span: Span,
        spec: &Type,
    ) -> Option<OpenSpecApplication> {
        let holes = match spec {
            Type::Named(path) => Some((path, &[][..])),
            Type::Generic(path, written) if written.contains(&GenericArg::Infer) => {
                Some((path, written.as_slice()))
            }
            _ => None,
        };
        if let Some((path, written)) = holes
            && let Some(declaration) = self.spec_declaration_for_path(path)
        {
            if written.is_empty() {
                let holes = vec![
                    None;
                    self.resolver
                        .types()
                        .spec_facts(&declaration)
                        .generics
                        .len()
                ];
                return Some((declaration, holes));
            }
            let args = self.written_spec_args(node_id, span, &declaration, written)?;
            return Some((declaration, args));
        }
        let (spec, args) = self.resolve_spec_reference(node_id, span, spec)?;
        Some((spec, args.into_iter().map(Some).collect()))
    }

    /// The spec `<target : _>::function` names: the one spec application
    /// declaring `function` among those `target` conforms to. Inherent
    /// functions are not candidates, because the slot names a spec. An
    /// application is its declaring spec with its generic arguments, as in
    /// `flatten_spec_into`, so distinct applications of one generic spec are
    /// distinct candidates.
    fn infer_qualified_spec(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &ResolvedType,
        function: &Ident,
    ) -> Option<(NominalRef, Vec<ResolvedGenericArg>)> {
        use crate::resolved_type::ResolvedSpecApplication;
        let conformances = match self.resolver.conformances_for_type(target) {
            Ok(conformances) => conformances,
            Err(err) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                return None;
            }
        };
        let mut found: Vec<ResolvedSpecApplication> = Vec::new();
        for conform in conformances {
            let declaring = self.without_diagnostics(|this| {
                this.flatten_spec(
                    node_id,
                    span,
                    &conform.spec,
                    &conform.spec_args,
                    &ResolvedType::Void,
                )
            });
            for declared in declaring.iter().flatten() {
                if &declared.name != function {
                    continue;
                }
                let application =
                    ResolvedSpecApplication::new(declared.spec.clone(), declared.spec_args());
                if !found.contains(&application) {
                    found.push(application);
                }
            }
        }
        match found.as_slice() {
            [application] => Some((application.spec.clone(), application.spec_args.clone())),
            [] => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::QualifiedSpecNotInferable {
                        target: target.to_string(),
                        function: function.clone(),
                    },
                );
                None
            }
            _ => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::QualifiedSpecAmbiguous {
                        target: target.to_string(),
                        function: function.clone(),
                        specs: distinct_spec_displays(&found),
                    },
                );
                None
            }
        }
    }

    fn require_requirement_visible(
        &mut self,
        node_id: HirId,
        span: Span,
        spec: &NominalRef,
        requirement: &RawSpecFunctionSig,
        origin: Origin,
    ) -> bool {
        let declaring_module = spec.module_path.clone();
        if self.check_visibility(requirement.visibility, &declaring_module, origin) {
            return true;
        }
        self.error(
            node_id,
            span,
            AnalysisErrorKind::MethodNotVisible {
                method: requirement.name.clone(),
                base: ResolvedType::Spec(spec.clone()),
            },
        );
        false
    }

    /// The functions every conformance of `target` to `spec` provides whose
    /// arguments agree with `spec_args`, where a hole agrees with anything.
    fn spec_call_methods(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &ResolvedType,
        spec: &NominalRef,
        spec_args: &[Option<ResolvedGenericArg>],
    ) -> Option<ConformingMethods> {
        if let Some(exact) = spec_args.iter().cloned().collect::<Option<Vec<_>>>() {
            return self.spec_call_conformance_methods(node_id, span, target, spec, &exact);
        }
        let conformances = match self.resolver.conformances_for_type(target) {
            Ok(conformances) => conformances,
            Err(err) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                return None;
            }
        };
        let spec_id = spec.decl_id;
        let matching: Vec<_> = conformances
            .into_iter()
            .filter(|conform| {
                conform.spec.decl_id == spec_id
                    && conform.spec_args.len() == spec_args.len()
                    && conform
                        .spec_args
                        .iter()
                        .zip(spec_args)
                        .all(|(actual, written)| written.as_ref().is_none_or(|w| w == actual))
            })
            .collect();
        if matching.is_empty() {
            let missing = self
                .resolver
                .types()
                .spec_facts(spec)
                .functions
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            self.error(
                node_id,
                span,
                AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
                    type_name: target.to_string(),
                    spec: spec.name.clone(),
                    missing,
                }),
            );
            return None;
        }
        Some(conforming_methods(matching))
    }

    fn spec_call_conformance_methods(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &ResolvedType,
        spec: &NominalRef,
        spec_args: &[ResolvedGenericArg],
    ) -> Option<ConformingMethods> {
        match self.resolver.conformance_for(target, spec, spec_args) {
            Ok(Some(conform)) => Some(conforming_methods(vec![conform])),
            Ok(None)
                if self
                    .type_implements_spec(node_id, span, target, spec, spec_args, false)
                    .is_ok() =>
            {
                match self.resolver.conformances_for_type(target) {
                    Ok(conformances) => Some(conforming_methods(conformances)),
                    Err(err) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                        None
                    }
                }
            }
            Ok(None) => {
                let missing = self
                    .resolver
                    .types()
                    .spec_facts(spec)
                    .functions
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
                        type_name: target.to_string(),
                        spec: spec.name.clone(),
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

    fn provider_labels(
        target: &ResolvedType,
        providers: &[String],
        method_name: &Ident,
    ) -> Vec<String> {
        providers
            .iter()
            .map(|provider| format!("<{target} : {provider}>::{}", method_name.as_ref()))
            .collect()
    }

    fn resolve_static_spec_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        spec: &NominalRef,
        spec_args: &[Option<ResolvedGenericArg>],
        method_name: &Ident,
        return_type: &Type,
        target: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(target) = target else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::SpecStaticNeedsExpectedType {
                    spec: spec.name.clone(),
                    function: method_name.clone(),
                },
            );
            return Intercepted::Claimed(None);
        };
        let returns_self = matches!(
            return_type,
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "Self"
        );
        if !returns_self {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::SpecStaticReturnNotSelf {
                    spec: spec.name.clone(),
                    function: method_name.clone(),
                    return_type: crate::error::raw_type_display(return_type),
                },
            );
            return Intercepted::Claimed(None);
        }
        let Some(conformance_methods) =
            self.spec_call_methods(node_id, span, target, spec, spec_args)
        else {
            return Intercepted::Claimed(None);
        };
        let (candidates, providers): (Vec<_>, Vec<_>) = conformance_methods
            .into_iter()
            .filter(|(name, method, _)| name == method_name && method.fn_type.self_mode.is_none())
            .map(|(_, method, provider)| (method, provider))
            .unzip();
        if candidates.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec.name.clone(),
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
                let Some(checked) = self.analyze_expr(arg, Expected::Exact(expected)) else {
                    ok = false;
                    continue;
                };
                let checked = self.coerce_to_expected(Some(expected), checked);
                if !Self::value_type_compatible(expected, &checked.r#type) {
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
            let Some((winner, checked_args)) = self.resolve_overload(
                node_id,
                span,
                method_name,
                &signatures,
                &Self::provider_labels(target, &providers, method_name),
                &call.args,
            ) else {
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
        spec: &NominalRef,
        spec_args: &[Option<ResolvedGenericArg>],
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
            self.spec_call_methods(node_id, span, &target, spec, spec_args)
        else {
            return Intercepted::Claimed(None);
        };
        let (candidates, providers): (Vec<_>, Vec<_>) = conformance_methods
            .into_iter()
            .filter(|(name, method, _)| name == method_name && method.fn_type.self_mode.is_some())
            .map(|(_, method, provider)| (method, provider))
            .unzip();
        if candidates.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchSpecFunction {
                    spec: spec.name.clone(),
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
                } else if let Some(checked) = self.analyze_expr(arg, Expected::Exact(expected)) {
                    checked
                } else {
                    ok = false;
                    continue;
                };
                let checked = self.coerce_to_expected(Some(expected), checked);
                if !Self::value_type_compatible(expected, &checked.r#type) {
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
            let Some((winner, checked_args)) = self.resolve_overload(
                node_id,
                span,
                method_name,
                &signatures,
                &Self::provider_labels(&target, &providers, method_name),
                &call.args,
            ) else {
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

/// Each application as written in source, module-qualified when two would
/// otherwise read the same, in a deterministic order.
fn distinct_spec_displays(
    applications: &[crate::resolved_type::ResolvedSpecApplication],
) -> Vec<String> {
    let mut displays = spec_displays(applications);
    displays.sort();
    displays
}

/// Each application as written in source, module-qualified when two would
/// otherwise read the same, in the order given.
fn spec_displays(applications: &[crate::resolved_type::ResolvedSpecApplication]) -> Vec<String> {
    let plain: Vec<String> = applications.iter().map(ToString::to_string).collect();
    applications
        .iter()
        .zip(&plain)
        .map(|(application, display)| {
            if plain.iter().filter(|other| *other == display).count() > 1 {
                let spec = &*application.spec;
                let mut qualified = spec.module_path.join(spec.name.clone()).to_string();
                if !application.spec_args.is_empty() {
                    let args = application
                        .spec_args
                        .iter()
                        .map(|arg| match arg {
                            ResolvedGenericArg::Type(ty) => {
                                crate::resolved_type::QualifiedType(ty).to_string()
                            }
                            ResolvedGenericArg::Comp(value) => value.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    qualified.push_str(&format!("<{args}>"));
                }
                qualified
            } else {
                display.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolved_type::ResolvedSpecApplication;
    use crate::type_store::{NominalKind, SpecFacts, testing};

    #[test]
    fn ambiguous_spec_displays_qualify_generic_arguments() {
        let spec = testing::define_spec(
            testing::identity(0, NominalKind::Spec, "P", &["pkg"], vec![]),
            SpecFacts {
                visibility: omega_parser::prelude::Visibility::Exposed,
                generics: vec![],
                is_object_safe: true,
                functions: vec![],
                suppress: vec![],
            },
        );
        let application = |module: &str, local| {
            let ty = testing::define_struct(
                testing::identity(local, NominalKind::Struct, "X", &[module], vec![]),
                StructFacts {
                    fields: vec![],
                    layout: crate::annotations::Layout::default(),
                    suppress: vec![],
                    is_marker: true,
                },
            );
            ResolvedSpecApplication::new(spec.clone(), vec![ResolvedGenericArg::Type(ty)])
        };
        let a = application("a", 1);
        let b = application("b", 2);
        assert_eq!(distinct_spec_displays(&[a.clone()]), vec!["P<X>"]);
        assert_eq!(
            distinct_spec_displays(&[b.clone(), a.clone()]),
            vec!["pkg::P<a::X>", "pkg::P<b::X>"]
        );
        assert_eq!(
            distinct_spec_displays(&[a.clone(), b.clone()]),
            distinct_spec_displays(&[b, a])
        );
    }
}
