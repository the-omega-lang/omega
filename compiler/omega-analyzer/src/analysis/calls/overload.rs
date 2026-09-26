use super::*;
use crate::resolver::UnmetBound;

/// A candidate that survived argument matching and proved applicable.
struct Viable {
    index: usize,
    score: u32,
    bindings: Vec<Option<ResolvedGenericArg>>,
    unbounded: Vec<usize>,
}

pub(crate) enum Applicability {
    Applies,
    Rejected(Rejection),
    /// A genuine error was reported; selection stops rather than treating
    /// the candidate as merely unsuitable.
    Failed,
}

/// Why a candidate that fit the arguments is still not the declaration this
/// call names. Kept until the whole set is known, so a failure reports every
/// reason at once rather than one losing candidate's error.
pub(crate) enum Rejection {
    Selector(usize),
    UnmetBound(UnmetBound),
    Undetermined(Ident),
}

impl Rejection {
    pub(crate) fn explain(&self) -> String {
        match self {
            Self::Selector(_) => " -- declares different bounds".to_string(),
            Self::UnmetBound(unmet) => format!(
                " -- '{}' does not implement '{}'",
                unmet.r#type,
                unmet.spec.as_ref()
            ),
            Self::Undetermined(parameter) => {
                format!(" -- '{}' is not determined here", parameter.as_ref())
            }
        }
    }
}

impl<'r> Analyzer<'r> {
    pub(crate) fn resolve_type_qualified_overload_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Expected<'_>,
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
        let chosen = &candidates[winner];
        if !self.check_visibility(chosen.visibility, &chosen.declaring_module, path.origin) {
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
        self.candidate_set(accessor, access, origin, false)
    }

    /// The same set for an *uncalled* reference, which a lone declaration
    /// also belongs to. See [`ModuleResolver::function_value_candidates`].
    pub(crate) fn function_value_set(
        &mut self,
        accessor: &[Ident],
        access: &ItemAccess,
        origin: Origin,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError> {
        self.candidate_set(accessor, access, origin, true)
    }

    fn candidate_set(
        &mut self,
        accessor: &[Ident],
        access: &ItemAccess,
        origin: Origin,
        values: bool,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError> {
        let revealed = self.reveals.allows(origin);
        let access = ItemAccess {
            absolute: access.absolute.clone(),
            bypass_visibility: access.bypass_visibility || revealed,
        };
        let set = if values {
            self.resolver.function_value_candidates(accessor, &access)?
        } else {
            self.resolver.resolve_overload_set(accessor, &access)?
        };
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
        _expected: Expected<'_>,
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
                declaring_module: Vec::new(),
            })
            .collect();
        let (winner, _, args) =
            self.resolve_overload_candidates(node_id, span, name, &candidates, args, Expected::None, &[], 0)?;
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
        expected: Expected<'_>,
        explicit: &[ExprGenericArg],
        implicit: usize,
    ) -> Option<(usize, Option<ResolvedMethod>, Vec<CheckedExprNode>)> {
        use crate::generics::pattern::TypePattern;
        // What the caller wrote is settled before any declaration is looked
        // at, so an invalid selector is this call's error rather than a
        // reason to keep trying candidates.
        let validated = self.validate_written_generics(node_id, span, explicit)?;
        let mut fixed = Vec::with_capacity(args.len());
        for arg in args {
            fixed.push(if Self::adaptable_literal(arg) {
                None
            } else {
                Some(self.analyze_expr(arg, Expected::None)?)
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
        let mut viable: Vec<Viable> = Vec::new();
        let mut rejected: Vec<(usize, Rejection)> = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let mut bindings = Vec::new();
            let mut written = WrittenGenerics::default();
            let patterns = if let Some(template) = candidate.template() {
                if template.params.len() != args.len() + implicit {
                    continue;
                }
                let template = template.clone();
                if validated.len() > template.generics.len() {
                    continue;
                }
                let Some(prefix) =
                    self.bind_written_generics(node_id, span, &validated, &template.generics, false)
                else {
                    continue;
                };
                written = prefix;
                bindings.resize(template.generics.len(), None);
                for (slot, binding) in bindings.iter_mut().zip(&written.bindings) {
                    slot.clone_from(binding);
                }
                if let Some(expected) = expected.exact() {
                    template.return_type.infer(expected, &mut bindings);
                }
                for (position, pattern) in template.params[implicit..].iter().enumerate() {
                    pattern.infer(&argument_type(position), &mut bindings);
                }
                let undetermined = template
                    .generics
                    .iter()
                    .zip(&bindings)
                    .position(|(param, binding)| binding.is_none() && param.default.is_none());
                if let Some(position) = undetermined {
                    if written.selectors.get(position).is_some_and(Option::is_some) {
                        rejected.push((
                            index,
                            Rejection::Undetermined(template.generics[position].ident.clone()),
                        ));
                    }
                    continue;
                }
                if !self.canonicalize_comp_bindings(&template, &mut bindings) {
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
            let Some(score) = score else { continue };
            // Applicability is decided before cost: a declaration whose
            // bounds this call cannot satisfy is not a cheaper answer, it is
            // not an answer at all.
            match self.candidate_applies(node_id, span, candidate, &written, &bindings) {
                Applicability::Failed => return None,
                Applicability::Rejected(reason) => rejected.push((index, reason)),
                Applicability::Applies => viable.push(Viable {
                    index,
                    score,
                    bindings,
                    unbounded: Self::unbounded_positions(candidate, &written),
                }),
            }
        }
        let Some(minimum) = viable.iter().map(|entry| entry.score).min() else {
            self.report_no_match(node_id, span, name, candidates, explicit, &rejected);
            return None;
        };
        viable.retain(|entry| entry.score == minimum);
        let dominates = |left: &Viable, right: &Viable| match (
            candidates[left.index].template(),
            candidates[right.index].template(),
        ) {
            (None, Some(_)) => true,
            (Some(_), Some(_)) => WrittenGenerics::dominates(&left.unbounded, &right.unbounded),
            _ => false,
        };
        let winners: Vec<&Viable> = viable
            .iter()
            .filter(|entry| !viable.iter().any(|other| dominates(other, entry)))
            .collect();
        let [winner] = winners.as_slice() else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: winners
                        .iter()
                        .map(|entry| Self::describe_candidate(&candidates[entry.index]))
                        .collect(),
                },
            );
            return None;
        };
        let (winner, bindings) = (winner.index, winner.bindings.clone());
        let instantiated = if candidates[winner].template().is_some() {
            match self
                .resolver
                .instantiate_overload(candidates[winner].decl_id, &bindings)
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
                None => self.analyze_expr(raw, Expected::Exact(expected))?,
            };
            final_args.push(self.coerce_to_expected(Some(expected), checked));
        }
        Some((winner, instantiated, final_args))
    }

    /// Whether a candidate can be the declaration this call names: its
    /// written selectors must name exactly the bounds it declares, and the
    /// arguments must prove every one of them.
    ///
    /// Only the declaration's bounds and the defaults needed to reach them
    /// are resolved here; the declaration itself stays unmaterialized, so a
    /// candidate that goes on to lose leaves no instantiation behind.
    pub(super) fn candidate_applies(
        &mut self,
        node_id: HirId,
        span: Span,
        candidate: &OverloadCandidate,
        written: &WrittenGenerics,
        bindings: &[Option<ResolvedGenericArg>],
    ) -> Applicability {
        if candidate.template().is_none() {
            return Applicability::Applies;
        }
        let prepared = match self.resolver.prepare_generic_call(
            crate::resolver::GenericCallTarget::Declaration(candidate.decl_id),
            bindings,
        ) {
            Ok(Some(prepared)) => prepared,
            // A resolver that cannot prepare declarations leaves selection
            // with what the templates themselves say.
            Ok(None) => return Applicability::Applies,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return Applicability::Failed;
            }
        };
        for (position, selector) in written.selectors.iter().enumerate() {
            let Some(selector) = selector else { continue };
            let declared = prepared.bounds.get(position).map_or(&[][..], Vec::as_slice);
            if !selector.matches(declared) {
                return Applicability::Rejected(Rejection::Selector(position));
            }
        }
        match prepared.unmet {
            Some(unmet) => Applicability::Rejected(Rejection::UnmetBound(unmet)),
            None => Applicability::Applies,
        }
    }

    /// The written plain-type positions at which a candidate declares no
    /// bounds. Emptiness survives substitution, so the declaration's own
    /// pattern is enough to decide it.
    fn unbounded_positions(candidate: &OverloadCandidate, written: &WrittenGenerics) -> Vec<usize> {
        let Some(template) = candidate.template() else {
            return Vec::new();
        };
        written.unbounded_positions(|position| {
            !template
                .bounds
                .iter()
                .any(|(parameter, _)| *parameter == position)
        })
    }

    /// The error a written selector earns when it identified one declaration
    /// and that declaration's own bound is what failed. `false` means no
    /// selector settled the question, so the ordinary list is the answer.
    pub(super) fn report_selected_bound_failure(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        explicit: &[ExprGenericArg],
        rejected: &[(usize, Rejection)],
    ) -> bool {
        if explicit
            .iter()
            .all(|entry| entry.selector_bounds().is_none())
        {
            return false;
        }
        let mut unmet = None;
        for (_, reason) in rejected {
            match reason {
                Rejection::Selector(_) => {}
                Rejection::UnmetBound(bound) if unmet.is_none() => unmet = Some(bound),
                _ => return false,
            }
        }
        let Some(unmet) = unmet else { return false };
        self.error(
            node_id,
            span,
            AnalysisErrorKind::SelectedBoundNotSatisfied {
                name: name.clone(),
                parameter: unmet.parameter.clone(),
                r#type: unmet.r#type.to_string(),
                spec: unmet.spec.clone(),
            },
        );
        true
    }

    fn report_no_match(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[OverloadCandidate],
        explicit: &[ExprGenericArg],
        rejected: &[(usize, Rejection)],
    ) {
        if self.report_selected_bound_failure(node_id, span, name, explicit, rejected) {
            return;
        }
        // A written selector that nothing declares is its own failure: the
        // ordinary "no overload matches" list would invite reading the call
        // as an argument mismatch.
        if let Some((selector, span)) = rejected.iter().find_map(|(_, reason)| {
            let Rejection::Selector(position) = reason else {
                return None;
            };
            let entry = explicit.get(*position)?;
            Some((
                entry.selector_bounds()?,
                entry.selector_span().unwrap_or(span),
            ))
        }) && rejected.iter().all(|(_, reason)| matches!(reason, Rejection::Selector(_)))
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoMatchingBoundSelector {
                    name: name.clone(),
                    selector: selector
                        .iter()
                        .map(crate::error::raw_type_display)
                        .collect::<Vec<_>>()
                        .join(" + "),
                    candidates: candidates.iter().map(Self::describe_candidate).collect(),
                },
            );
            return;
        }
        let described = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let description = Self::describe_candidate(candidate);
                match rejected.iter().find(|(rejected, _)| *rejected == index) {
                    Some((_, reason)) => format!("{description}{}", reason.explain()),
                    None => description,
                }
            })
            .collect();
        self.error(
            node_id,
            span,
            AnalysisErrorKind::NoMatchingOverload {
                name: name.clone(),
                candidates: described,
            },
        );
    }

    fn describe_candidate(candidate: &OverloadCandidate) -> String {
        candidate
            .template()
            .map(|t| t.description.clone())
            .unwrap_or_else(|| {
                ResolvedType::Function(candidate.fn_type().cloned().unwrap()).to_string()
            })
    }

    /// Rewrites every inferred `comp` binding into its declared parameter
    /// type, which is the authoritative one, and reports whether each value
    /// is exactly representable there. `false` means this candidate cannot be
    /// instantiated with these bindings at all.
    pub(super) fn canonicalize_comp_bindings(
        &self,
        template: &OverloadTemplate,
        bindings: &mut [Option<ResolvedGenericArg>],
    ) -> bool {
        let mut ok = true;
        for position in 0..bindings.len() {
            let Some(ResolvedGenericArg::Comp(value)) = &bindings[position] else {
                continue;
            };
            let kind = template.comp_types[position]
                .as_ref()
                .and_then(|pattern| pattern.resolved(bindings))
                .and_then(|ty| CompScalarType::from_resolved(&ty));
            match (value, kind) {
                (CompScalar::Int { value, .. }, Some(CompScalarType::Int(kind))) => {
                    let value = *value;
                    let Some((min, max)) =
                        kind.resolved().integer_domain(self.target.pointer_bits())
                    else {
                        ok = false;
                        continue;
                    };
                    if !(min..=max).contains(&value) {
                        ok = false;
                    }
                    bindings[position] = Some(ResolvedGenericArg::Comp(CompScalar::Int {
                        r#type: kind,
                        value,
                    }));
                }
                (CompScalar::Bool(_), Some(CompScalarType::Bool))
                | (CompScalar::Char(_), Some(CompScalarType::Char)) => {}
                _ => ok = false,
            }
        }
        ok
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
