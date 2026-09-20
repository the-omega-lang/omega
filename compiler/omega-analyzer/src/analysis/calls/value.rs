use super::*;

/// What an uncalled reference to a function name resolved to.
pub(crate) enum FunctionValue {
    Selected {
        /// Index of the winning candidate, so a caller can recover the
        /// declaration's visibility without re-deriving which one won.
        candidate: usize,
        decl_id: HirId,
        fn_type: ResolvedFunctionType,
    },
    /// Selection failed, and the reason was reported.
    Failed,
    /// One generic declaration is all this name offers, and nothing about the
    /// reference determines its arguments. The diagnostic belongs to the
    /// caller: which spellings could have supplied them differs between a
    /// bare name, a module-qualified path, and a member path.
    Undetermined,
}

/// A candidate that survived the written generic arguments, together with
/// what they bound.
type Prepared = (usize, Vec<Option<ResolvedGenericArg>>);

impl<'r> Analyzer<'r> {
    /// Picks the one declaration an uncalled reference names, instantiating
    /// it when it is generic.
    ///
    /// `declared` is the absolute path whose generic parameters `explicit`
    /// applies to, used only to report a written argument list that does not
    /// fit. A non-empty `explicit` restricts selection to generic
    /// declarations: writing arguments is how a reference says it means a
    /// template, so a concrete declaration of the same name is not a
    /// fallback.
    pub(crate) fn select_function_value(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        declared: &[Ident],
        candidates: &[OverloadCandidate],
        explicit: &[GenericArg],
        expected: Option<&ResolvedType>,
    ) -> FunctionValue {
        let eligible: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| explicit.is_empty() || candidate.template().is_some())
            .map(|(index, _)| index)
            .collect();
        // A lone eligible candidate is not a speculative one: a written
        // argument list that does not fit it is reported against it, rather
        // than taken as evidence that it was not the declaration meant.
        let report = eligible.len() == 1;

        let mut prepared: Vec<Prepared> = Vec::new();
        for &index in &eligible {
            let Some(template) = candidates[index].template().cloned() else {
                prepared.push((index, Vec::new()));
                continue;
            };
            match self.explicit_bindings(node_id, span, declared, &template, explicit, report) {
                Some(bindings) => prepared.push((index, bindings)),
                None if report => return FunctionValue::Failed,
                None => {}
            }
        }

        match expected {
            Some(ResolvedType::Function(expected)) => self.select_against(
                node_id, span, name, candidates, &eligible, prepared, expected,
            ),
            _ => self.select_unconstrained(node_id, span, name, candidates, prepared, explicit),
        }
    }

    /// Selection against a known function type: every survivor must have
    /// exactly that signature, and specificity breaks the remaining tie.
    #[allow(clippy::too_many_arguments)]
    fn select_against(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        eligible: &[usize],
        prepared: Vec<Prepared>,
        expected: &ResolvedFunctionType,
    ) -> FunctionValue {
        // One concrete declaration is not a choice. Selecting it and letting
        // the surrounding context report an ordinary type mismatch says more
        // than "no declaration has this type" would.
        if let [(index, _)] = prepared.as_slice()
            && candidates[*index].template().is_none()
        {
            let index = *index;
            return self.instantiate_function_value(
                node_id,
                span,
                name,
                candidates,
                index,
                &[],
                None,
            );
        }
        let mut matched: Vec<Prepared> = Vec::new();
        for (index, bindings) in prepared {
            match candidates[index].template() {
                None => {
                    if Self::value_signature(&candidates[index]) == *expected {
                        matched.push((index, bindings));
                    }
                }
                Some(template) => {
                    let template = template.clone();
                    if let Some(bindings) = self.match_template(&template, bindings, expected) {
                        matched.push((index, bindings));
                    }
                }
            }
        }

        let winners: Vec<usize> = matched
            .iter()
            .map(|(index, _)| *index)
            .filter(|index| {
                !matched
                    .iter()
                    .any(|(other, _)| Self::value_dominates(candidates, *other, *index))
            })
            .collect();
        let [winner] = winners.as_slice() else {
            if winners.is_empty() {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NoMatchingFunctionValue {
                        name: name.clone(),
                        expected: ResolvedType::Function(expected.clone()).to_string(),
                        candidates: eligible
                            .iter()
                            .map(|index| Self::describe_value_candidate(&candidates[*index]))
                            .collect(),
                    },
                );
            } else {
                self.ambiguous_value(node_id, span, name, candidates, &winners);
            }
            return FunctionValue::Failed;
        };
        let bindings = matched
            .into_iter()
            .find(|(index, _)| index == winner)
            .expect("the winner came from the matched set")
            .1;
        self.instantiate_function_value(
            node_id,
            span,
            name,
            candidates,
            *winner,
            &bindings,
            Some(expected),
        )
    }

    /// Selection with no expected function type. Nothing here can infer a
    /// generic argument, so a reference resolves only when what was written
    /// already leaves one usable declaration.
    fn select_unconstrained(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        prepared: Vec<Prepared>,
        explicit: &[GenericArg],
    ) -> FunctionValue {
        if explicit.is_empty() {
            // An uncalled reference with no expected type still excludes
            // generic declarations: nothing here determines their arguments.
            let concrete: Vec<usize> = prepared
                .iter()
                .map(|(index, _)| *index)
                .filter(|index| candidates[*index].template().is_none())
                .collect();
            return match (concrete.as_slice(), prepared.len()) {
                ([only], _) => self.instantiate_function_value(
                    node_id,
                    span,
                    name,
                    candidates,
                    *only,
                    &[],
                    None,
                ),
                ([], 0 | 1) => FunctionValue::Undetermined,
                ([], _) => {
                    let among: Vec<usize> = prepared.iter().map(|(index, _)| *index).collect();
                    self.ambiguous_value(node_id, span, name, candidates, &among);
                    FunctionValue::Failed
                }
                _ => {
                    self.ambiguous_value(node_id, span, name, candidates, &concrete);
                    FunctionValue::Failed
                }
            };
        }

        let [(index, bindings)] = prepared.as_slice() else {
            if prepared.is_empty() {
                return FunctionValue::Undetermined;
            }
            let among: Vec<usize> = prepared.iter().map(|(index, _)| *index).collect();
            self.ambiguous_value(node_id, span, name, candidates, &among);
            return FunctionValue::Failed;
        };
        // Everything the written prefix left open must come from the
        // declaration's own defaults: there is no other information here.
        let template = candidates[*index]
            .template()
            .expect("a written argument list keeps only templates");
        if let Some(parameter) = template
            .generics
            .iter()
            .zip(bindings)
            .find(|(param, binding)| binding.is_none() && param.default.is_none())
            .map(|(param, _)| param.ident.clone())
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::UndeterminedFunctionValue {
                    name: name.clone(),
                    parameter,
                },
            );
            return FunctionValue::Failed;
        }
        let (index, bindings) = (*index, bindings.clone());
        self.instantiate_function_value(node_id, span, name, candidates, index, &bindings, None)
    }

    /// The generic arguments that give a template exactly `expected` as its
    /// value signature, starting from what was written explicitly.
    fn match_template(
        &mut self,
        template: &OverloadTemplate,
        mut bindings: Vec<Option<ResolvedGenericArg>>,
        expected: &ResolvedFunctionType,
    ) -> Option<Vec<Option<ResolvedGenericArg>>> {
        if template.params.len() != expected.params.len()
            || template.calling_convention != expected.calling_convention
            || template.is_variadic != expected.is_variadic
        {
            return None;
        }
        // Written arguments are already bound and inference never replaces a
        // binding, so a conflicting occurrence leaves a type the exact match
        // below rejects rather than silently re-choosing one.
        template
            .return_type
            .infer(&expected.return_type, &mut bindings);
        for (pattern, found) in template.params.iter().zip(expected.param_types()) {
            pattern.infer(found, &mut bindings);
        }
        if !self.canonicalize_comp_bindings(template, &mut bindings) {
            return None;
        }
        if !template
            .return_type
            .identical(&expected.return_type, &bindings)
            || !template
                .params
                .iter()
                .zip(expected.param_types())
                .all(|(pattern, found)| pattern.identical(found, &bindings))
        {
            return None;
        }
        // A parameter the signature never mentions may still be completed
        // from its default; one without a default leaves the declaration
        // unusable here. No default ever establishes the match itself.
        template
            .generics
            .iter()
            .zip(&bindings)
            .all(|(param, binding)| binding.is_some() || param.default.is_some())
            .then_some(bindings)
    }

    /// Binds the written positional prefix of a template's generic
    /// parameters. With `report`, a list that does not fit the declaration is
    /// diagnosed; without it the candidate is merely rejected.
    fn explicit_bindings(
        &mut self,
        node_id: HirId,
        span: Span,
        declared: &[Ident],
        template: &OverloadTemplate,
        explicit: &[GenericArg],
        report: bool,
    ) -> Option<Vec<Option<ResolvedGenericArg>>> {
        let mut bindings = vec![None; template.generics.len()];
        if explicit.is_empty() {
            return Some(bindings);
        }
        if report {
            let resolved = self.resolve_generic_arg_list(
                node_id,
                span,
                explicit,
                declared,
                &template.generics,
            )?;
            for (slot, arg) in bindings.iter_mut().zip(resolved) {
                *slot = Some(arg);
            }
            return Some(bindings);
        }
        if explicit.len() > template.generics.len() {
            return None;
        }
        for (position, written) in explicit.iter().enumerate() {
            let reveals = &self.reveals;
            let resolved = self
                .context
                .resolve_generic_arg(
                    written,
                    Some(&template.generics[position]),
                    self.resolver,
                    &self.module_path,
                    ResolveItemOptions::INDIRECT,
                    &|origin| reveals.allows(origin),
                )
                .ok()?;
            bindings[position] = Some(resolved);
        }
        Some(bindings)
    }

    /// Materializes the selected declaration and checks the signature it
    /// actually produced. Only the winner is instantiated, so a losing
    /// generic's body, bounds, and defaults are never analyzed.
    #[allow(clippy::too_many_arguments)]
    fn instantiate_function_value(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        index: usize,
        bindings: &[Option<ResolvedGenericArg>],
        expected: Option<&ResolvedFunctionType>,
    ) -> FunctionValue {
        let (decl_id, fn_type) = match candidates[index].template() {
            None => (
                candidates[index].decl_id,
                Self::value_signature(&candidates[index]),
            ),
            Some(_) => match self
                .resolver
                .instantiate_overload(candidates[index].decl_id, bindings)
            {
                Ok(method) => (method.decl_id, method.value_fn_type()),
                Err(error) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                    return FunctionValue::Failed;
                }
            },
        };
        if let Some(expected) = expected
            && fn_type != *expected
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoMatchingFunctionValue {
                    name: name.clone(),
                    expected: ResolvedType::Function(expected.clone()).to_string(),
                    candidates: vec![ResolvedType::Function(fn_type).to_string()],
                },
            );
            return FunctionValue::Failed;
        }
        FunctionValue::Selected {
            candidate: index,
            decl_id,
            fn_type,
        }
    }

    /// Whether `left` is strictly more specific than `right`, by the same
    /// rule calls use: a concrete declaration beats a generic one, and among
    /// generics the stricter bound set wins. Parameter structure never
    /// participates.
    fn value_dominates(candidates: &[OverloadCandidate], left: usize, right: usize) -> bool {
        match (candidates[left].template(), candidates[right].template()) {
            (None, Some(_)) => true,
            (Some(left), Some(right)) => {
                crate::generics::compare_bound_sets(&left.bounds, &right.bounds)
                    == Some(std::cmp::Ordering::Greater)
            }
            _ => false,
        }
    }

    /// A concrete candidate's type as a first-class value: the receiver of a
    /// member declaration is an ordinary parameter here.
    fn value_signature(candidate: &OverloadCandidate) -> ResolvedFunctionType {
        candidate
            .fn_type()
            .expect("a candidate is either a template or a signature")
            .unbound_value()
    }

    fn ambiguous_value(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        among: &[usize],
    ) {
        self.error(
            node_id,
            span,
            AnalysisErrorKind::AmbiguousOverload {
                name: name.clone(),
                candidates: among
                    .iter()
                    .map(|index| Self::describe_value_candidate(&candidates[*index]))
                    .collect(),
            },
        );
    }

    /// A candidate as a diagnostic names it. A concrete member is described
    /// by its unbound value type, which is the type this reference would
    /// have produced.
    fn describe_value_candidate(candidate: &OverloadCandidate) -> String {
        match candidate.template() {
            Some(template) => template.description.clone(),
            None => ResolvedType::Function(Self::value_signature(candidate)).to_string(),
        }
    }
}
