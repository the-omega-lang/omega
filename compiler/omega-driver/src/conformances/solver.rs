use super::*;
use omega_diagnostics::SourceSpan;

impl Driver {
    pub(crate) fn conformance_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Option<ConformanceEntry> {
        let matches = |entry: &&ConformanceEntry| {
            entry.target == target.lookup_key()
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
        };
        if let Some(entry) = self.conformances.entries.iter().find(matches) {
            return Some(entry.clone());
        }
        // Goal-directed: instantiate only the templates that can produce
        // this spec, then look again. A goal still on the stack after that
        // is a genuine cycle, reported with the chain that closes it.
        self.solve(target, Some(&spec.borrow().id));
        if let Some(entry) = self.conformances.entries.iter().find(matches) {
            return Some(entry.clone());
        }
        if let Some(active) = self
            .conformances
            .goals
            .iter()
            .find(|goal| goal.target == target.lookup_key() && goal.spec == spec.borrow().id)
        {
            if self
                .conformances
                .reported_cycles
                .iter()
                .any(|(t, id)| *t == target.lookup_key() && *id == spec.borrow().id)
            {
                return None;
            }
            self.conformances
                .reported_cycles
                .push((target.lookup_key(), spec.borrow().id));
            let goals: Vec<(String, Ident, ModulePath, Span)> = self
                .conformances
                .goals
                .iter()
                .map(|goal| {
                    (
                        goal.target.to_string(),
                        goal.spec_name.clone(),
                        goal.module.clone(),
                        goal.span,
                    )
                })
                .collect();
            let active_module = active.module.clone();
            let active_span = active.span;
            let active_id = active.id;
            let mut chain: Vec<(String, Ident, Option<SourceSpan>)> = goals
                .into_iter()
                .map(|(target, spec, module, span)| (target, spec, self.site(&module, span)))
                .collect();
            chain.push((
                target.to_string(),
                spec.borrow().name.clone(),
                self.site(&active_module, active_span),
            ));
            self.diagnostics.error(
                &self
                    .conformances
                    .templates
                    .iter()
                    .find(|template| template.conform.id == active.id)
                    .map(|template| template.module.clone())
                    .unwrap_or_default(),
                AnalysisError::new(
                    active_id,
                    active_span,
                    AnalysisErrorKind::ConformanceCycle {
                        target: target.to_string(),
                        spec: spec.borrow().name.clone(),
                        chain,
                    },
                ),
            );
        }
        None
    }

    pub(crate) fn bound_context_for(
        &mut self,
        concrete: &ResolvedType,
        spec: Rc<RefCell<ResolvedSpecType>>,
        spec_args: Vec<ResolvedType>,
        declared_keys: &[(HirId, Vec<ResolvedType>)],
    ) -> Vec<ResolvedBound> {
        let mut permitted = HashSet::new();
        permitted.insert(spec.borrow().id);
        let mut seeds = vec![ResolvedBound::new(concrete.clone(), spec, spec_args)];
        for entry in self.conformances_for_type(concrete) {
            if permitted.contains(&entry.spec.borrow().id) {
                seeds.push(ResolvedBound::new(
                    entry.target.clone(),
                    entry.spec.clone(),
                    entry.spec_args.clone(),
                ));
                continue;
            }
            if entry.origin == ConformanceOrigin::Concrete {
                continue;
            }
            // Both sides compare alias-expanded key sets: the candidate's
            // stored `declared_bound_keys` against the item's own expanded
            // `declared_keys`.
            let entailed = entry
                .declared_bound_keys
                .iter()
                .all(|bound| declared_keys.contains(bound));
            if entailed {
                seeds.push(ResolvedBound::new(
                    entry.target.clone(),
                    entry.spec.clone(),
                    entry.spec_args.clone(),
                ));
            }
        }
        seeds
    }

    pub(crate) fn bound_context_over(
        &mut self,
        declared: &[ResolvedBound],
        declared_keys: &[(HirId, Vec<ResolvedType>)],
    ) -> Vec<ResolvedBound> {
        let mut context = Vec::new();
        for bound in declared {
            context.extend(self.bound_context_for(
                &bound.target,
                bound.spec.clone(),
                bound.spec_args.clone(),
                declared_keys,
            ));
        }
        context
    }

    pub(crate) fn conformances_for_type(&mut self, target: &ResolvedType) -> Vec<ConformanceEntry> {
        let key = target.lookup_key();
        if !self.conformances.materialized.contains(&key) {
            let outcome = self.solve(target, None);
            // A partial sweep is not a complete one: if any template was
            // skipped because its goal was already in flight, the memo must
            // not claim completeness -- the next query re-sweeps.
            if !outcome.skipped_goal {
                self.conformances.materialized.push(key);
            }
        }
        self.conformances
            .entries
            .iter()
            .filter(|entry| entry.target == target.lookup_key())
            .cloned()
            .collect()
    }

    fn with_conformance_goal<T>(
        &mut self,
        goal: ConformanceGoal,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.conformances.goals.push(goal);
        let result = f(self);
        self.conformances
            .goals
            .pop()
            .expect("conformance goal just pushed");
        result
    }

    pub(crate) fn solve(&mut self, target: &ResolvedType, spec: Option<&HirId>) -> SweepOutcome {
        let templates = self.conformances.templates.clone();
        let mut skipped_goal = false;
        for template in templates {
            let Some(substitution) = Self::match_conform_target(&template.conform, target) else {
                continue;
            };
            if self
                .conformances
                .failed
                .iter()
                .any(|(id, failed)| *id == template.conform.id && *failed == target.lookup_key())
            {
                continue;
            }
            let Some((produced, _)) = self.template_spec(&template, &substitution) else {
                // The produced spec does not resolve. The demand path skips
                // silently; the full sweep still instantiates and reports.
                if spec.is_some() {
                    continue;
                }
                self.instantiate_conformance(
                    &template.module,
                    &template.conform,
                    &substitution,
                    template.origin,
                );
                continue;
            };
            let spec_id = produced.borrow().id;
            let spec_name = produced.borrow().name.clone();
            if let Some(wanted) = spec
                && spec_id != *wanted
            {
                continue;
            }
            if self
                .conformances
                .goals
                .iter()
                .any(|goal| goal.target == target.lookup_key() && goal.spec == spec_id)
            {
                skipped_goal = true;
                continue;
            }
            let goal = ConformanceGoal {
                id: template.conform.id,
                target: target.lookup_key(),
                spec: spec_id,
                spec_name,
                module: template.module.clone(),
                span: template.conform.span,
            };
            self.with_conformance_goal(goal, |this| {
                this.instantiate_conformance(
                    &template.module,
                    &template.conform,
                    &substitution,
                    template.origin,
                )
            });
        }
        SweepOutcome { skipped_goal }
    }

    fn template_spec(
        &mut self,
        template: &ConformanceTemplate,
        substitution: &[(Ident, ResolvedType)],
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        self.with_analyzer(
            &template.module,
            substitution,
            AnalysisSite::new(template.conform.id, template.conform.span),
            |analyzer| {
                analyzer.without_diagnostics(|a| {
                    a.resolve_spec_reference(
                        template.conform.id,
                        template.conform.span,
                        &template.conform.spec,
                    )
                })
            },
        )
        .result
    }

    pub(super) fn unconstrained_parameter(conform: &HirConformDef) -> Option<Ident> {
        let mut mentioned = Vec::new();
        Self::collect_type_idents(&conform.target, &mut mentioned);
        conform
            .generics
            .iter()
            .find(|generic| !mentioned.contains(&generic.ident))
            .map(|generic| generic.ident.clone())
    }

    fn collect_type_idents(r#type: &Type, out: &mut Vec<Ident>) {
        match r#type {
            Type::Named(path) => {
                if path.is_unqualified() {
                    out.push(path.head.clone());
                }
            }
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => Self::collect_type_idents(inner, out),
            Type::Generic(_, args) => {
                for arg in args {
                    Self::collect_type_idents(arg, out);
                }
            }
            _ => {}
        }
    }

    fn match_conform_target(
        conform: &HirConformDef,
        actual: &ResolvedType,
    ) -> Option<Vec<(Ident, ResolvedType)>> {
        if let Type::Named(path) = &conform.target
            && path.is_unqualified()
            && conform
                .generics
                .iter()
                .any(|generic| generic.ident == path.head)
        {
            return Analyzer::is_conformable_target(actual)
                .then(|| vec![(path.head.clone(), actual.clone())]);
        }
        if let (Type::InferredArray(raw_item), ResolvedType::Slice { item, .. }) =
            (&conform.target, actual)
        {
            let Type::Named(path) = raw_item.as_ref() else {
                return None;
            };
            if !path.is_unqualified()
                || !conform
                    .generics
                    .iter()
                    .any(|generic| generic.ident == path.head)
            {
                return None;
            }
            return Some(vec![(path.head.clone(), (**item).clone())]);
        }
        let (actual_name, actual_args) = match actual {
            ResolvedType::Struct(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            ResolvedType::Enum { cell, .. } => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            ResolvedType::Union(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            _ => return None,
        };
        let (path, args) = match &conform.target {
            Type::Generic(path, args) => (path, args),
            _ => return None,
        };
        if path.segments().last() != Some(&actual_name) || args.len() != actual_args.len() {
            return None;
        }
        let mut substitution = Vec::new();
        for (raw, concrete) in args.iter().zip(actual_args) {
            let Type::Named(path) = raw else { return None };
            if !path.is_unqualified()
                || !conform
                    .generics
                    .iter()
                    .any(|generic| generic.ident == path.head)
            {
                return None;
            }
            substitution.push((path.head.clone(), concrete));
        }
        Some(substitution)
    }
}
