use super::*;
use omega_analyzer::resolver::ImportTarget;

impl Driver {
    pub(crate) fn mark_bound_type_imports(
        &mut self,
        module: &[Ident],
        generics: &[HirGenericParam],
    ) {
        let mut seen = HashSet::new();
        for param in generics {
            for bound in &param.bounds {
                self.mark_type_import_dependencies(module, bound, &mut seen);
            }
            if let Some(default) = &param.default {
                self.mark_type_import_dependencies(module, default, &mut seen);
            }
        }
    }

    /// Marks every import a written type consumes, exactly as if its final
    /// alias-expanded spelling had appeared directly: an alias RHS, an
    /// alias-owned bound, or an alias-owned default all count as if written
    /// at the use site, and a chained alias is followed through every link.
    fn mark_type_import_dependencies(
        &mut self,
        module: &[Ident],
        ty: &Type,
        seen: &mut HashSet<(ModulePath, Ident)>,
    ) {
        match ty {
            // An anchored path names its target directly and consumes no
            // import; every other head is a binding this module owns.
            Type::Named(path) | Type::Generic(path, _) if path.anchor.is_none() => {
                let _ = self.import_entry(module, &path.head);
                if path.tail.is_empty() {
                    self.mark_alias_declaration_import_dependencies(module, &path.head, seen);
                }
            }
            _ => {}
        }
        match ty {
            Type::Generic(_, args) | Type::SpecStatic(args) | Type::AnonymousEnum(args) => {
                for arg in args {
                    self.mark_type_import_dependencies(module, arg, seen);
                }
            }
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => self.mark_type_import_dependencies(module, inner, seen),
            Type::Function(f) => {
                for param in &f.params {
                    self.mark_type_import_dependencies(module, &param.r#type, seen);
                }
                self.mark_type_import_dependencies(module, &f.return_type, seen);
            }
            Type::Named(_) => {}
        }
    }

    /// If `name` names a declared alias reachable from `module` -- locally or
    /// through an import -- marks every import the alias's own declaration
    /// (its target, bounds, and defaults) consumes, in the alias's own
    /// declaration module. A module importing an alias directly still marks
    /// that alias import itself as used (see the caller); this accounts for
    /// what the alias's *declaration* separately depends on, which is owed
    /// to the alias's own module, not the accessor's.
    pub(crate) fn mark_alias_declaration_import_dependencies(
        &mut self,
        module: &[Ident],
        name: &Ident,
        seen: &mut HashSet<(ModulePath, Ident)>,
    ) {
        let (alias_module, alias_name) = if matches!(self.alias_index(module, name), Ok(Some(_))) {
            (module.to_vec(), name.clone())
        } else {
            let imported = self.resolve_import_alias_entry(module, name);
            let Ok(Some(ImportTarget::ItemPath(access))) = imported else {
                return;
            };
            let Some((alias_name, alias_module)) = access.absolute.split_last() else {
                return;
            };
            (alias_module.to_vec(), alias_name.clone())
        };
        // Keyed by the alias's own declaration rather than by the name a use
        // site happened to bind it to, so a chain that revisits a link stops
        // here instead of recursing forever.
        if !seen.insert((alias_module.clone(), alias_name.clone())) {
            return;
        }
        // An alias this module may not name, or one whose own declaration is
        // invalid, contributes no dependency: a reference that will be
        // rejected must not make the alias module's imports count as used.
        let Ok(Some(_)) = self.visible_alias(module, &alias_module, &alias_name, false) else {
            return;
        };
        let Ok(Some(index)) = self.alias_index(&alias_module, &alias_name) else {
            return;
        };
        let HirItem::Alias(declared) = &self.modules.parsed(&alias_module).hir.items[index] else {
            return;
        };
        let declared = declared.clone();
        let written = match &declared.target {
            AliasTarget::Path(path) => Type::Named(path.clone()),
            AliasTarget::Type(r#type) => r#type.clone(),
        };
        self.mark_type_import_dependencies(&alias_module, &written, seen);
        for param in &declared.generics {
            for bound in &param.bounds {
                self.mark_type_import_dependencies(&alias_module, bound, seen);
            }
            if let Some(default) = &param.default {
                self.mark_type_import_dependencies(&alias_module, default, seen);
            }
        }
    }

    pub(crate) fn collect_conformance_signatures(&mut self, paths: &[ModulePath]) {
        let mut concrete = Vec::new();
        for module in paths {
            let declarations: Vec<_> = self
                .modules
                .parsed(module)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Conform(conform) => Some(conform.clone()),
                    _ => None,
                })
                .collect();
            for conform in declarations {
                self.mark_bound_type_imports(module, &conform.generics);
                let Some(origin) = ConformanceOrigin::classify(&conform.target, &conform.generics)
                else {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::ConformTargetNotAType,
                        ),
                    );
                    continue;
                };
                if let Some(parameter) = Self::unconstrained_parameter(&conform) {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::UnconstrainedConformanceParameter { parameter },
                        ),
                    );
                    continue;
                }
                if origin == ConformanceOrigin::Blanket {
                    let spec_run = self.with_analyzer(
                        module,
                        &[],
                        AnalysisSite::new(conform.id, conform.span),
                        |analyzer| {
                            analyzer.resolve_spec_reference(conform.id, conform.span, &conform.spec)
                        },
                    );
                    self.diagnostics.record_warnings(module, spec_run.warnings);
                    let Some((spec, _)) = spec_run.result else {
                        continue;
                    };
                    let spec_package = spec
                        .borrow()
                        .module_path
                        .first()
                        .cloned()
                        .unwrap_or_else(|| Ident(String::new()));
                    if module.first() != Some(&spec_package) {
                        self.diagnostics.error(
                            module,
                            AnalysisError::new(
                                conform.id,
                                conform.span,
                                AnalysisErrorKind::BlanketConformanceForeignSpec { spec_package },
                            ),
                        );
                        continue;
                    }
                }
                match origin {
                    ConformanceOrigin::Concrete => concrete.push((module.clone(), conform)),
                    ConformanceOrigin::Generic | ConformanceOrigin::Blanket => {
                        self.conformances.templates.push(ConformanceTemplate {
                            module: module.clone(),
                            conform,
                            origin,
                        });
                    }
                }
            }
        }
        // Every template is visible before a concrete conform can cause a
        // bound lookup, removing module-order dependence.
        for (module, conform) in concrete {
            self.instantiate_conformance(&module, &conform, &[], ConformanceOrigin::Concrete);
        }
    }

    pub(super) fn instantiate_conformance(
        &mut self,
        module: &[Ident],
        conform: &HirConformDef,
        substitution: &[(Ident, ResolvedType)],
        origin: ConformanceOrigin,
    ) -> Option<ConformanceEntry> {
        let target_run = self.with_analyzer(
            module,
            substitution,
            AnalysisSite::new(conform.id, conform.span),
            |analyzer| analyzer.resolve_conform_target(conform.id, conform.span, &conform.target),
        );
        self.diagnostics
            .record_warnings(module, target_run.warnings);
        let target = target_run.result?;
        let spec_run = self.with_analyzer(
            module,
            substitution,
            AnalysisSite::new(conform.id, conform.span),
            |analyzer| analyzer.resolve_spec_reference(conform.id, conform.span, &conform.spec),
        );
        self.diagnostics.record_warnings(module, spec_run.warnings);
        let spec_reference = spec_run.result?;
        let type_args: Vec<_> = conform
            .generics
            .iter()
            .map(|param| {
                substitution
                    .iter()
                    .find(|(ident, _)| ident == &param.ident)
                    .map(|(_, r#type)| r#type.clone())
                    .expect("a generic conform template pins every parameter")
            })
            .collect();
        // Checked before the success guard below, because a failure
        // registers no entry for that guard to find.
        if self
            .conformances
            .failed
            .iter()
            .any(|(id, failed)| *id == conform.id && *failed == target.lookup_key())
        {
            return None;
        }
        // The recursion guard lives in `solve`: it pushes this
        // instantiation's `(target, spec)` goal and skips any template
        // already in flight, so re-entry is impossible here -- only
        // `conformance_for` reports a cycle, once the goal stack shows the
        // proof closed on itself.
        let declared_bounds = match self.check_generic_bounds(
            module,
            AnalysisSite::new(conform.id, conform.span),
            &conform.generics,
            &type_args,
        ) {
            Some(Ok(bounds)) => bounds,
            Some(Err(error)) => {
                // At the outermost goal, the failure is genuine and
                // permanent, worth memoizing in `failed`. A nested failure
                // is not: the in-flight proof above it may itself fail and
                // unwind, and the same template may be re-asked later from a
                // clean stack.
                if self.conformances.goals.len() == 1 {
                    self.conformances
                        .failed
                        .push((conform.id, target.lookup_key()));
                }
                // A blanket's bound is its applicability predicate: a
                // non-`Animal` type simply does not receive
                // `conform<T: Animal> T ...`. Generic constructor templates
                // still diagnose a matched `List<NotBound>` as an invalid
                // instantiation.
                if origin != ConformanceOrigin::Blanket {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::ModuleResolution(error),
                        ),
                    );
                }
                return None;
            }
            None => {
                if self.conformances.goals.len() == 1 {
                    self.conformances
                        .failed
                        .push((conform.id, target.lookup_key()));
                }
                return None;
            }
        };
        // Instantiating one template twice at the same target is not a
        // duplicate conform -- `conformances_for_type` re-walks every
        // matching template on each call, so without this the second lookup
        // would report `DuplicateConformance` against its own first entry.
        if let Some(existing) =
            self.conformances.entries.iter().find(|existing| {
                existing.id == conform.id && existing.target == target.lookup_key()
            })
        {
            return Some(existing.clone());
        }
        let mut method_substitution = substitution.to_vec();
        method_substitution.push((Ident("Self".to_string()), target.clone()));
        // The declared set's alias-expanded identity -- both blanket
        // precedence and derived-conformance admission compare on this, so
        // an alias bound and its inline spelling are interchangeable.
        let keys_run = self.with_analyzer(
            module,
            &substitution,
            AnalysisSite::new(conform.id, conform.span),
            |a| a.expand_bound_set(conform.id, conform.span, &declared_bounds),
        );
        self.diagnostics.record_warnings(module, keys_run.warnings);
        let declared_bound_keys = keys_run.result;
        // Resolve precedence before checking the potentially expensive body.
        // In particular, a blanket superseded by an explicit conform must not
        // surface diagnostics from a body that can never be emitted.
        let header = ConformanceEntry {
            module: module.to_vec(),
            id: conform.id,
            span: conform.span,
            target: target.lookup_key(),
            spec: spec_reference.0.clone(),
            spec_args: spec_reference.1.clone(),
            methods: vec![],
            method_ids: vec![],
            functions: vec![],
            pending: vec![],
            substitution: method_substitution.clone(),
            declared_bounds: declared_bounds.clone(),
            declared_bound_keys: declared_bound_keys.clone(),
            origin,
        };
        // Keep the established diagnostic order: an illegal foreign conform
        // is rejected for violating the orphan rule, even if an imported
        // declaration happens to own the same conformance key.
        if !self.check_conformance_orphan(&header) {
            return None;
        }
        if self.registration_decision(&header) == RegistrationDecision::Ignore {
            return None;
        }
        let method_ids =
            self.conformance_method_ids(module, conform.id, &target, &conform.functions);
        let run = self.with_analyzer(
            module,
            &method_substitution,
            AnalysisSite::new(conform.id, conform.span),
            |analyzer| {
                analyzer.check_conform_block(
                    conform.id,
                    conform.span,
                    &target,
                    &spec_reference,
                    &conform.functions,
                    &method_ids,
                )
            },
        );
        self.diagnostics.record_warnings(module, run.warnings);
        let (spec, spec_args, methods, pending) = run.result?;
        let entry = ConformanceEntry {
            module: module.to_vec(),
            id: conform.id,
            span: conform.span,
            target: target.lookup_key(),
            spec,
            spec_args,
            methods,
            method_ids,
            functions: conform.functions.clone(),
            pending,
            substitution: method_substitution,
            declared_bounds,
            declared_bound_keys,
            origin,
        };
        if !self.register_conformance(entry.clone()) {
            return None;
        }
        Some(entry)
    }

    fn check_conformance_orphan(&mut self, entry: &ConformanceEntry) -> bool {
        let local = entry
            .module
            .first()
            .cloned()
            .unwrap_or_else(|| Ident(String::new()));
        let target_package = entry
            .target
            .declaring_owner()
            .and_then(|(path, _)| path.first().cloned())
            .unwrap_or_else(|| Ident("core".to_string()));
        let spec_package = entry
            .spec
            .borrow()
            .module_path
            .first()
            .cloned()
            .unwrap_or_else(|| Ident(String::new()));
        if local == target_package || local == spec_package {
            return true;
        }
        self.diagnostics.error(
            &entry.module,
            AnalysisError::new(
                entry.id,
                entry.span,
                AnalysisErrorKind::ConformanceOrphanViolation {
                    target_package,
                    spec_package,
                },
            ),
        );
        false
    }

    fn register_conformance(&mut self, entry: ConformanceEntry) -> bool {
        match self.registration_decision(&entry) {
            RegistrationDecision::Insert => {
                self.conformances.entries.push(entry);
                true
            }
            RegistrationDecision::Replace(index) => {
                self.conformances.entries.remove(index);
                self.conformances.entries.push(entry);
                true
            }
            RegistrationDecision::Ignore => false,
        }
    }

    fn registration_decision(&mut self, entry: &ConformanceEntry) -> RegistrationDecision {
        let incumbent = self.conformances.entries.iter().position(|existing| {
            existing.target == entry.target
                && existing.spec.borrow().id == entry.spec.borrow().id
                && existing.spec_args == entry.spec_args
        });
        let Some(index) = incumbent else {
            return RegistrationDecision::Insert;
        };
        let existing = self.conformances.entries[index].clone();
        match Self::compare_conformance_precedence(entry, &existing) {
            Some(Ordering::Greater) => RegistrationDecision::Replace(index),
            Some(Ordering::Less) => RegistrationDecision::Ignore,
            Some(Ordering::Equal) => {
                self.diagnostics.error(
                    &entry.module,
                    AnalysisError::new(
                        entry.id,
                        entry.span,
                        AnalysisErrorKind::DuplicateConformance {
                            target: entry.target.to_string(),
                            spec: entry.spec.borrow().name.clone(),
                            previous: existing.span,
                        },
                    ),
                );
                RegistrationDecision::Ignore
            }
            None => {
                self.diagnostics.error(
                    &entry.module,
                    AnalysisError::new(
                        entry.id,
                        entry.span,
                        AnalysisErrorKind::AmbiguousConformance {
                            target: entry.target.to_string(),
                            spec: entry.spec.borrow().name.clone(),
                            first: existing.span,
                        },
                    ),
                );
                RegistrationDecision::Ignore
            }
        }
    }

    fn compare_conformance_precedence(
        candidate: &ConformanceEntry,
        incumbent: &ConformanceEntry,
    ) -> Option<Ordering> {
        if candidate.origin == ConformanceOrigin::Blanket
            && incumbent.origin == ConformanceOrigin::Blanket
        {
            // Both sides compare alias-expanded key sets, so `T: AB` and
            // `T: A + B` compare as equal.
            let candidate_subset_of_incumbent = candidate
                .declared_bound_keys
                .iter()
                .all(|bound| incumbent.declared_bound_keys.contains(bound));
            let incumbent_subset_of_candidate = incumbent
                .declared_bound_keys
                .iter()
                .all(|bound| candidate.declared_bound_keys.contains(bound));
            return match (candidate_subset_of_incumbent, incumbent_subset_of_candidate) {
                (true, false) => Some(Ordering::Less),
                (false, true) => Some(Ordering::Greater),
                (true, true) => Some(Ordering::Equal),
                (false, false) => None,
            };
        }
        Some(candidate.precedence().cmp(&incumbent.precedence()))
    }
}
