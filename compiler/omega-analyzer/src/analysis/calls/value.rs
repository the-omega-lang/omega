use super::*;
use overload::{Applicability, Rejection};

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
/// what they bound and what they require of it.
struct Prepared {
    index: usize,
    written: WrittenGenerics,
    bindings: Vec<Option<ResolvedGenericArg>>,
}

/// The survivors of applicability, and why each of the rest was ruled out.
type Applicable = (Vec<Prepared>, Vec<(usize, Rejection)>);

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
        explicit: &[ExprGenericArg],
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

        // The selector names and alias obligations the reference wrote are
        // the caller's, whatever this name turns out to offer.
        let Some(validated) = self.validate_written_generics(node_id, span, explicit) else {
            return FunctionValue::Failed;
        };

        let mut prepared: Vec<Prepared> = Vec::new();
        for &index in &eligible {
            let Some(template) = candidates[index].template().cloned() else {
                prepared.push(Prepared {
                    index,
                    written: WrittenGenerics::default(),
                    bindings: Vec::new(),
                });
                continue;
            };
            match self.explicit_bindings(node_id, span, declared, &template, &validated, report) {
                Some((written, bindings)) => prepared.push(Prepared {
                    index,
                    written,
                    bindings,
                }),
                None if report => return FunctionValue::Failed,
                None => {}
            }
        }

        match expected {
            Some(ResolvedType::Function(expected)) => self.select_against(
                node_id, span, name, candidates, &eligible, prepared, explicit, expected,
            ),
            _ => self.select_unconstrained(node_id, span, name, candidates, prepared, explicit),
        }
    }

    /// Selection against a known function type: every survivor must have
    /// exactly that signature, prove its own bounds, and satisfy whatever
    /// the written prefix selected. The unbounded preference breaks the
    /// remaining tie.
    #[allow(clippy::too_many_arguments)]
    fn select_against(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        eligible: &[usize],
        prepared: Vec<Prepared>,
        explicit: &[ExprGenericArg],
        expected: &ResolvedFunctionType,
    ) -> FunctionValue {
        // One concrete declaration is not a choice. Selecting it and letting
        // the surrounding context report an ordinary type mismatch says more
        // than "no declaration has this type" would.
        if let [only] = prepared.as_slice()
            && candidates[only.index].template().is_none()
        {
            let index = only.index;
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
        for entry in prepared {
            match candidates[entry.index].template() {
                None => {
                    if Self::value_signature(&candidates[entry.index]) == *expected {
                        matched.push(entry);
                    }
                }
                Some(template) => {
                    let template = template.clone();
                    let Some(bindings) =
                        self.match_template(&template, entry.bindings.clone(), expected)
                    else {
                        continue;
                    };
                    matched.push(Prepared { bindings, ..entry });
                }
            }
        }
        let Some((applicable, rejected)) =
            self.applicable_values(node_id, span, candidates, matched)
        else {
            return FunctionValue::Failed;
        };

        let winners: Vec<usize> = applicable
            .iter()
            .filter(|entry| {
                !applicable
                    .iter()
                    .any(|other| Self::value_dominates(candidates, other, entry))
            })
            .map(|entry| entry.index)
            .collect();
        let [winner] = winners.as_slice() else {
            if winners.is_empty() {
                if self.report_selected_bound_failure(node_id, span, name, explicit, &rejected) {
                    return FunctionValue::Failed;
                }
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NoMatchingFunctionValue {
                        name: name.clone(),
                        expected: ResolvedType::Function(expected.clone()).to_string(),
                        candidates: Self::describe_value_candidates(
                            candidates, eligible, &rejected,
                        ),
                    },
                );
            } else {
                self.ambiguous_value(node_id, span, name, candidates, &winners);
            }
            return FunctionValue::Failed;
        };
        let bindings = applicable
            .into_iter()
            .find(|entry| entry.index == *winner)
            .expect("the winner came from the matched set")
            .bindings;
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

    /// Drops the candidates whose declared bounds this reference cannot
    /// prove, or whose bounds a written selector does not name, keeping why.
    /// `None` means a genuine error was reported while deciding.
    fn applicable_values(
        &mut self,
        node_id: HirId,
        span: Span,
        candidates: &[OverloadCandidate],
        prepared: Vec<Prepared>,
    ) -> Option<Applicable> {
        let mut applicable = Vec::with_capacity(prepared.len());
        let mut rejected = Vec::new();
        for entry in prepared {
            match self.candidate_applies(
                node_id,
                span,
                &candidates[entry.index],
                &entry.written,
                &entry.bindings,
            ) {
                Applicability::Applies => applicable.push(entry),
                Applicability::Rejected(reason) => rejected.push((entry.index, reason)),
                Applicability::Failed => return None,
            }
        }
        Some((applicable, rejected))
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
        explicit: &[ExprGenericArg],
    ) -> FunctionValue {
        if explicit.is_empty() {
            // An uncalled reference with no expected type still excludes
            // generic declarations: nothing here determines their arguments.
            let concrete: Vec<usize> = prepared
                .iter()
                .map(|entry| entry.index)
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
                    let among: Vec<usize> = prepared.iter().map(|entry| entry.index).collect();
                    self.ambiguous_value(node_id, span, name, candidates, &among);
                    FunctionValue::Failed
                }
                _ => {
                    self.ambiguous_value(node_id, span, name, candidates, &concrete);
                    FunctionValue::Failed
                }
            };
        }

        // Everything the written prefix left open must come from the
        // declaration's own defaults: there is no other information here.
        // A list that fits no declaration at all determines nothing, which is
        // the caller's diagnostic rather than this one's.
        let fits_nothing = prepared.is_empty();
        let mut determined = Vec::with_capacity(prepared.len());
        let mut undetermined = None;
        for entry in prepared {
            let template = candidates[entry.index]
                .template()
                .expect("a written argument list keeps only templates");
            match template
                .generics
                .iter()
                .zip(&entry.bindings)
                .position(|(param, binding)| binding.is_none() && param.default.is_none())
            {
                None => determined.push(entry),
                Some(position) => {
                    let parameter = template.generics[position].ident.clone();
                    undetermined.get_or_insert((entry.written.is_hole(position), parameter));
                }
            }
        }
        let Some((applicable, rejected)) =
            self.applicable_values(node_id, span, candidates, determined)
        else {
            return FunctionValue::Failed;
        };
        let winners: Vec<usize> = applicable
            .iter()
            .filter(|entry| {
                !applicable
                    .iter()
                    .any(|other| Self::value_dominates(candidates, other, entry))
            })
            .map(|entry| entry.index)
            .collect();
        let [winner] = winners.as_slice() else {
            if !winners.is_empty() {
                self.ambiguous_value(node_id, span, name, candidates, &winners);
                return FunctionValue::Failed;
            }
            return match undetermined {
                Some((true, parameter)) => {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UndeterminedInferenceHole {
                            name: name.clone(),
                            parameter,
                        },
                    );
                    FunctionValue::Failed
                }
                Some((false, parameter)) => {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UndeterminedFunctionValue {
                            name: name.clone(),
                            parameter,
                        },
                    );
                    FunctionValue::Failed
                }
                None if fits_nothing => FunctionValue::Undetermined,
                None if self.report_selected_bound_failure(
                    node_id, span, name, explicit, &rejected,
                ) => FunctionValue::Failed,
                None => {
                    let all: Vec<usize> = (0..candidates.len()).collect();
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::NoMatchingOverload {
                            name: name.clone(),
                            candidates: Self::describe_value_candidates(
                                candidates, &all, &rejected,
                            ),
                        },
                    );
                    FunctionValue::Failed
                }
            };
        };
        let bindings = applicable
            .into_iter()
            .find(|entry| entry.index == *winner)
            .expect("the winner came from the applicable set")
            .bindings;
        self.instantiate_function_value(node_id, span, name, candidates, *winner, &bindings, None)
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
    /// parameters, keeping whatever bound selectors it wrote. With `report`,
    /// a list that does not fit the declaration is diagnosed; without it the
    /// candidate is merely rejected.
    fn explicit_bindings(
        &mut self,
        node_id: HirId,
        span: Span,
        declared: &[Ident],
        template: &OverloadTemplate,
        explicit: &ValidatedGenerics,
        report: bool,
    ) -> Option<(WrittenGenerics, Vec<Option<ResolvedGenericArg>>)> {
        let mut bindings: Vec<Option<ResolvedGenericArg>> = vec![None; template.generics.len()];
        if explicit.is_empty() {
            return Some((WrittenGenerics::default(), bindings));
        }
        if report {
            self.check_generic_arity(node_id, span, declared, &template.generics, explicit.len())?;
        } else if explicit.len() > template.generics.len() {
            return None;
        }
        let written =
            self.bind_written_generics(node_id, span, explicit, &template.generics, report)?;
        for (slot, binding) in bindings.iter_mut().zip(&written.bindings) {
            slot.clone_from(binding);
        }
        Some((written, bindings))
    }

    /// Materializes the selected declaration and checks the signature it
    /// actually produced. Only the winner is instantiated, so no losing
    /// generic's body is ever analyzed.
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
    /// generics the one left unbounded at strictly more of the caller's
    /// written plain-type positions wins. Parameter structure and bound
    /// strength never participate.
    fn value_dominates(candidates: &[OverloadCandidate], left: &Prepared, right: &Prepared) -> bool {
        match (
            candidates[left.index].template(),
            candidates[right.index].template(),
        ) {
            (None, Some(_)) => true,
            (Some(left_template), Some(right_template)) => WrittenGenerics::dominates(
                &left.written.unbounded_positions(|position| {
                    !left_template
                        .bounds
                        .iter()
                        .any(|(parameter, _)| *parameter == position)
                }),
                &right.written.unbounded_positions(|position| {
                    !right_template
                        .bounds
                        .iter()
                        .any(|(parameter, _)| *parameter == position)
                }),
            ),
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

    /// The candidate list a failure reports, each carrying whatever reason
    /// ruled it out. Naming the reason beside the candidate is what keeps a
    /// failed bound visible now that it no longer stops selection.
    fn describe_value_candidates(
        candidates: &[OverloadCandidate],
        among: &[usize],
        rejected: &[(usize, Rejection)],
    ) -> Vec<String> {
        among
            .iter()
            .map(|index| {
                let description = Self::describe_value_candidate(&candidates[*index]);
                match rejected.iter().find(|(rejected, _)| rejected == index) {
                    Some((_, reason)) => format!("{description}{}", reason.explain()),
                    None => description,
                }
            })
            .collect()
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
