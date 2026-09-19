use super::*;

impl<'r> Analyzer<'r> {
    pub(crate) fn resolve_type_qualified_overload_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(expr_path) = Self::callee_expr_path(call) else {
            return Intercepted::Declined;
        };
        let path = &expr_path.path;
        let (namespace, member, segments) = match path.tail.as_slice() {
            [.., segment, member] if segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT => {
                (FunctionNamespace::Member, member, 2)
            }
            [.., member] => (FunctionNamespace::Static, member, 1),
            [] => return Intercepted::Declined,
        };
        let owner_generics =
            !expr_path.generic_args.is_empty() && expr_path.args_at + segments == path.tail.len();
        let function_generics =
            !expr_path.generic_args.is_empty() && expr_path.args_at == path.tail.len();
        if !expr_path.generic_args.is_empty() && !owner_generics && !function_generics {
            return Intercepted::Declined;
        }
        let Some(owner) =
            self.callee_owner_type(node_id, span, expr_path, segments, owner_generics)
        else {
            return Intercepted::Declined;
        };
        let candidates = match self
            .resolver
            .method_overload_candidates(&owner, member, namespace)
        {
            Ok(candidates) => candidates,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return Intercepted::Claimed(None);
            }
        };
        if candidates.len() < 2 {
            return Intercepted::Declined;
        }
        let explicit = if function_generics {
            expr_path.generic_args.as_slice()
        } else {
            &[]
        };
        let Some((winner, instantiated, args)) = self.resolve_overload_candidates(
            node_id,
            span,
            member,
            &candidates,
            &call.args,
            expected,
            explicit,
            0,
        ) else {
            return Intercepted::Claimed(None);
        };
        let (module, owner_id) = owner
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(
            candidates[winner].visibility,
            &module,
            owner_id,
            path.origin,
        ) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: owner,
                },
            );
            return Intercepted::Claimed(None);
        }
        let (decl_id, fn_type) = match instantiated {
            Some(method) => (method.decl_id, method.value_fn_type()),
            None => (
                candidates[winner].decl_id,
                candidates[winner]
                    .fn_type()
                    .cloned()
                    .unwrap()
                    .unbound_value(),
            ),
        };
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

    /// The candidate set a written overload name offers this caller. A
    /// use-site `reveal` is an explicit local bypass, so it joins whatever
    /// authorization the binding already carried; everything else about
    /// which candidates exist is the resolver's answer, not a rule
    /// reconstructed here.
    pub(crate) fn overload_set(
        &mut self,
        accessor: &[Ident],
        access: &ItemAccess,
        origin: Origin,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError> {
        let revealed = self.reveals.allows(origin);
        let access = ItemAccess {
            absolute: access.absolute.clone(),
            bypass_visibility: access.bypass_visibility || revealed,
        };
        let set = self.resolver.resolve_overload_set(accessor, &access)?;
        if revealed
            && let Some(set) = &set
            && let Some((_, module)) = set.absolute.split_last()
            && set
                .candidates
                .iter()
                .any(|candidate| !Self::visibility_allows(candidate.visibility, module, accessor))
        {
            self.reveals.mark_used();
        }
        Ok(set)
    }

    pub(crate) fn resolve_bare_overload_candidates(
        &mut self,
        ident: &Ident,
        origin: Origin,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError> {
        let accessor = self.origin_module(origin);
        let alias = self.resolver.resolve_import_alias(&accessor, ident)?;
        let access = match alias {
            Some(ImportTarget::ItemPath(access)) => access,
            Some(ImportTarget::Item(absolute, _)) => ItemAccess::gated(absolute),
            _ => ItemAccess::gated(
                accessor
                    .iter()
                    .cloned()
                    .chain(std::iter::once(ident.clone()))
                    .collect(),
            ),
        };
        self.overload_set(&accessor, &access, origin)
    }

    pub(crate) fn resolve_overloaded_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        _expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(expr_path) = Self::callee_expr_path(call) else {
            return Intercepted::Declined;
        };

        let path = &expr_path.path;
        if !expr_path.generic_args.is_empty() && expr_path.args_at != path.tail.len() {
            return Intercepted::Declined;
        }
        if path.is_unqualified()
            && self
                .context
                .find_variable(&path.head, path.origin)
                .is_some()
        {
            return Intercepted::Declined;
        }

        // Every spelling reaches the same already-authorized candidate set:
        // a bare (possibly aliased) name, an explicitly anchored path, and a
        // module-qualified path differ only in how the absolute path is
        // found, never in which candidates the caller may then choose
        // between.
        let set = if path.is_unqualified() {
            match self.resolve_bare_overload_candidates(&path.head, path.origin) {
                Ok(Some(set)) => set,
                Ok(None) => return Intercepted::Declined,
                Err(error) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                    return Intercepted::Claimed(None);
                }
            }
        } else {
            let accessor = self.path_module(path);
            let access = match self.module_qualified_path(node_id, span, path) {
                ModuleQualifiedPath::Item(access) => access,
                ModuleQualifiedPath::NotModule => return Intercepted::Declined,
                ModuleQualifiedPath::Failed => return Intercepted::Claimed(None),
            };
            match self.overload_set(&accessor, &access, path.origin) {
                Ok(Some(set)) => set,
                Ok(None) => return Intercepted::Declined,
                Err(e) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                    return Intercepted::Claimed(None);
                }
            }
        };
        let Some((name, _)) = Self::split_item_path(&set.absolute) else {
            return Intercepted::Declined;
        };
        let candidates = set.candidates;

        let Some((winner, instantiated, args)) = self.resolve_overload_candidates(
            node_id,
            span,
            &name,
            &candidates,
            &call.args,
            _expected,
            &expr_path.generic_args,
            0,
        ) else {
            return Intercepted::Claimed(None);
        };
        let (decl_id, fn_type) = match instantiated {
            Some(method) => (method.decl_id, method.fn_type),
            None => (
                candidates[winner].decl_id,
                candidates[winner].fn_type().cloned().unwrap(),
            ),
        };
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

    pub(super) fn resolve_overload(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[(HirId, ResolvedFunctionType)],
        args: &[HirExprNode],
    ) -> Option<(usize, Vec<CheckedExprNode>)> {
        let candidates: Vec<_> = candidates
            .iter()
            .map(|(decl_id, fn_type)| OverloadCandidate {
                decl_id: *decl_id,
                signature: crate::resolver::OverloadSignature::Concrete(fn_type.clone()),
                visibility: Visibility::Exposed,
            })
            .collect();
        let (winner, _, args) =
            self.resolve_overload_candidates(node_id, span, name, &candidates, args, None, &[], 0)?;
        Some((winner, args))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_overload_candidates(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        args: &[HirExprNode],
        expected: Option<&ResolvedType>,
        explicit: &[GenericArg],
        implicit: usize,
    ) -> Option<(usize, Option<ResolvedMethod>, Vec<CheckedExprNode>)> {
        use crate::generics::pattern::TypePattern;
        use std::cmp::Ordering;
        let mut fixed = Vec::with_capacity(args.len());
        for arg in args {
            fixed.push(if Self::adaptable_literal(arg) {
                None
            } else {
                Some(self.analyze_expr(arg, None)?)
            });
        }
        let argument_type = |index: usize| -> ResolvedType {
            if let Some(checked) = &fixed[index] {
                return checked.r#type.clone();
            }
            let raw = match &args[index].expr {
                HirExpr::Negate(inner) => &inner.expr,
                other => other,
            };
            match raw {
                HirExpr::Number(number) if number.fractional_part.is_some() => ResolvedType::F32,
                _ => ResolvedType::I32,
            }
        };
        let mut viable = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let mut bindings = Vec::new();
            let patterns = if let Some(template) = candidate.template() {
                if template.params.len() != args.len() + implicit
                    || explicit.len() > template.generics.len()
                {
                    continue;
                }
                bindings.resize(template.generics.len(), None);
                let mut explicit_ok = true;
                for (position, written) in explicit.iter().enumerate() {
                    let reveals = &self.reveals;
                    match self.context.resolve_generic_arg(
                        written,
                        Some(&template.generics[position]),
                        self.resolver,
                        &self.module_path,
                        ResolveItemOptions::INDIRECT,
                        &|origin| reveals.allows(origin),
                    ) {
                        Ok(value) => bindings[position] = Some(value),
                        Err(_) => {
                            explicit_ok = false;
                            break;
                        }
                    }
                }
                if !explicit_ok {
                    continue;
                }
                if let Some(expected) = expected {
                    template.return_type.infer(expected, &mut bindings);
                }
                for (position, pattern) in template.params[implicit..].iter().enumerate() {
                    pattern.infer(&argument_type(position), &mut bindings);
                }
                let mut complete = true;
                for (position, param) in template.generics.iter().enumerate() {
                    if bindings[position].is_none() && param.default.is_none() {
                        complete = false;
                    }
                    if let Some(ResolvedGenericArg::Comp(value)) = &bindings[position] {
                        let kind = template.comp_types[position]
                            .as_ref()
                            .and_then(|pattern| pattern.resolved(&bindings))
                            .and_then(|ty| CompScalarType::from_resolved(&ty));
                        match (value, kind) {
                            (CompScalar::Int { value, .. }, Some(CompScalarType::Int(kind))) => {
                                let value = *value;
                                let Some((min, max)) =
                                    kind.resolved().integer_domain(self.target.pointer_bits())
                                else {
                                    complete = false;
                                    continue;
                                };
                                if !(min..=max).contains(&value) {
                                    complete = false;
                                }
                                bindings[position] =
                                    Some(ResolvedGenericArg::Comp(CompScalar::Int {
                                        r#type: kind,
                                        value,
                                    }));
                            }
                            (CompScalar::Bool(_), Some(CompScalarType::Bool))
                            | (CompScalar::Char(_), Some(CompScalarType::Char)) => {}
                            _ => complete = false,
                        }
                    }
                }
                if !complete {
                    continue;
                }
                template.params[implicit..].to_vec()
            } else {
                let signature = candidate.fn_type().unwrap();
                if !explicit.is_empty()
                    || signature.is_variadic
                    || signature.params.len() != args.len() + implicit
                {
                    continue;
                }
                signature
                    .param_types()
                    .skip(implicit)
                    .cloned()
                    .map(TypePattern::Fixed)
                    .collect()
            };
            let score = patterns
                .iter()
                .enumerate()
                .try_fold(0, |score, (position, pattern)| {
                    let found = argument_type(position);
                    let cost = if let Some(target) = pattern.resolved(&bindings) {
                        if fixed[position].is_none() {
                            Self::literal_overload_fit(
                                &args[position],
                                &target,
                                self.target.pointer_bits(),
                            )
                            .map(|exact| u32::from(!exact))
                            .or_else(|| Self::literal_injection_fit(&args[position], &target))
                        } else {
                            Self::conversion_cost(&target, &found)
                        }
                    } else {
                        Self::pattern_conversion_cost(pattern, &found, &bindings)
                    };
                    cost.map(|cost| score + cost)
                });
            if let Some(score) = score {
                viable.push((index, score, bindings));
            }
        }
        let describe = |candidate: &OverloadCandidate| {
            candidate
                .template()
                .map(|t| t.description.clone())
                .unwrap_or_else(|| {
                    ResolvedType::Function(candidate.fn_type().cloned().unwrap()).to_string()
                })
        };
        let Some(minimum) = viable.iter().map(|(_, cost, _)| *cost).min() else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoMatchingOverload {
                    name: name.clone(),
                    candidates: candidates.iter().map(describe).collect(),
                },
            );
            return None;
        };
        viable.retain(|(_, cost, _)| *cost == minimum);
        let dominates = |left: usize, right: usize| match (
            candidates[left].template(),
            candidates[right].template(),
        ) {
            (None, Some(_)) => true,
            (Some(left), Some(right)) => {
                crate::generics::compare_bound_sets(&left.bounds, &right.bounds)
                    == Some(Ordering::Greater)
            }
            _ => false,
        };
        let winners: Vec<_> = viable
            .iter()
            .filter(|(index, _, _)| !viable.iter().any(|(other, _, _)| dominates(*other, *index)))
            .collect();
        let [(winner, _, bindings)] = winners.as_slice() else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: winners
                        .iter()
                        .map(|(index, _, _)| describe(&candidates[*index]))
                        .collect(),
                },
            );
            return None;
        };
        let winner = *winner;
        let instantiated = if candidates[winner].template().is_some() {
            match self
                .resolver
                .instantiate_overload(candidates[winner].decl_id, bindings)
            {
                Ok(method) => Some(method),
                Err(error) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                    return None;
                }
            }
        } else {
            None
        };
        let signature = instantiated
            .as_ref()
            .map(|m| &m.fn_type)
            .or(candidates[winner].fn_type())
            .unwrap();
        let parameters: Vec<_> = signature.param_types().skip(implicit).cloned().collect();
        let mut final_args = Vec::new();
        for ((raw, checked), expected) in args.iter().zip(fixed).zip(&parameters) {
            let checked = match checked {
                Some(checked) => checked,
                None => self.analyze_expr(raw, Some(expected))?,
            };
            final_args.push(self.coerce_to_expected(Some(expected), checked));
        }
        Some((winner, instantiated, final_args))
    }

    fn pattern_conversion_cost(
        pattern: &crate::generics::pattern::TypePattern,
        found: &ResolvedType,
        bindings: &[Option<ResolvedGenericArg>],
    ) -> Option<u32> {
        use crate::generics::pattern::TypePattern;
        if pattern.accepts(found, bindings) {
            return Some(0);
        }
        if let Some((_, member)) = found.refined_anonymous_member() {
            return Self::pattern_conversion_cost(pattern, member, bindings).map(|_| 2);
        }
        if matches!(pattern, TypePattern::AnonymousEnum(_)) {
            let leaves = pattern.leaves(bindings);
            let fits = |ty: &ResolvedType| {
                leaves
                    .iter()
                    .any(|leaf| leaf.exact(&ty.widened(), bindings))
            };
            return match found {
                ResolvedType::AnonymousEnum {
                    shape,
                    variant: None,
                } => shape.members().iter().all(fits).then_some(2),
                _ => fits(found).then_some(2),
            };
        }
        None
    }

    /// The cost of injecting an adaptable numeric literal into an
    /// anonymous-enum parameter, using the literal's ordinary default type.
    fn literal_injection_fit(arg: &HirExprNode, param_type: &ResolvedType) -> Option<u32> {
        let n = match &arg.expr {
            HirExpr::Number(n) => n,
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => n,
                _ => return None,
            },
            _ => return None,
        };
        let default = if n.fractional_part.is_some() {
            ResolvedType::F32
        } else {
            ResolvedType::I32
        };
        Self::conversion_cost(param_type, &default).filter(|&cost| cost > 0)
    }

    fn literal_overload_fit(
        arg: &HirExprNode,
        target: &ResolvedType,
        pointer_bits: u32,
    ) -> Option<bool> {
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
            ResolvedType::F32
        } else {
            ResolvedType::I32
        };
        Some(*target == default)
    }
}
