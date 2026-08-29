use super::*;

impl Driver {
    /// Visits every module whose prerequisites resolved. A poisoned module has
    /// no usable bindings, so its bodies are skipped rather than checked
    /// against fabricated data; the modules around it still report.
    pub(super) fn check_bodies(
        &mut self,
        local: &[ModulePath],
    ) -> (CheckedModules, TaggedWarnings) {
        let mut modules = Vec::with_capacity(local.len());
        let mut warnings = TaggedWarnings::new();

        for path in local {
            if self.diagnostics.is_poisoned(path) {
                continue;
            }
            let items = self.check_module_bodies(path, &mut warnings);
            let id = self.modules.parsed(path).id;
            modules.push((path.clone(), CheckedModule { id, items }));
        }

        (modules, warnings)
    }

    fn check_module_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let mut bodies: Vec<CheckedBody> = Vec::new();

        for (name, index) in self.modules.index(path).plain_items() {
            match self.is_generic_template(path, &name) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    self.record_item_failure(path, error);
                    continue;
                }
            }
            bodies.extend(self.ensure_item_body(&ItemKey::new(path, &name, &[]), index));
        }

        for indices in self.modules.index(path).overloads.clone().into_values() {
            for index in indices {
                bodies.extend(self.ensure_overload_body(path, index));
            }
        }

        let mut items = Vec::with_capacity(bodies.len());
        for body in bodies {
            items.push(body.item);
            warnings.extend(body.warnings.into_iter().map(|w| (path.to_vec(), w)));
        }
        items.extend(self.synthesize_gap_items(path));
        items.extend(self.check_glue_bodies(path, warnings));
        items.extend(self.check_primitive_bodies(path, warnings));
        items.extend(self.check_conformance_bodies(path, warnings));
        items
    }

    fn check_glue_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let signatures: Vec<(
            std::rc::Rc<omega_analyzer::resolved_type::ResolvedGap>,
            Vec<(omega_hir::HirFunctionDef, ResolvedFunctionType)>,
        )> = self
            .items
            .glues
            .iter()
            .filter(|glue| glue.module == path)
            .map(|glue| (glue.gap.clone(), glue.functions.clone()))
            .collect();
        let mut items = Vec::new();
        for (gap, functions) in signatures {
            for (function, fn_type) in functions {
                // Already-checked invariants, not filters -- an unregistered
                // glue never reaches here. Uses `same_glue_signature`, not
                // `!=`: `ResolvedFunctionType`'s derived `PartialEq` also
                // compares parameter names, which are a glue's own
                // implementation detail.
                let Some((_, declared)) = gap
                    .functions
                    .iter()
                    .find(|(name, _)| *name == function.name)
                else {
                    continue;
                };
                if !Self::same_glue_signature(&declared.fn_type, &fn_type) {
                    continue;
                }
                let annotations = omega_analyzer::annotations::ResolvedAnnotations {
                    mangling: ManglingMode::Glued {
                        spec_module_path: gap.module_path.clone(),
                        spec_name: gap.name.clone(),
                        function_name: function.name.clone(),
                    },
                    ..Default::default()
                };
                let run = self.with_analyzer(
                    path,
                    &[],
                    AnalysisSite::new(function.id, function.span),
                    |analyzer| {
                        analyzer.check_function_body(&function, &fn_type, function.id, &annotations)
                    },
                );
                if let Some(checked) = run.result {
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
        }
        items
    }

    fn check_conformance_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let entries: Vec<_> = self
            .conformances
            .entries
            .iter()
            .filter(|entry| {
                entry.module == path
                    && (!self.roots.is_extern(&entry.module)
                        || entry.origin != ConformanceOrigin::Concrete)
                    && !self.conformances.emitted.contains(&(
                        entry.target.clone(),
                        entry.spec.borrow().id,
                        entry.spec_args.clone(),
                    ))
            })
            .cloned()
            .collect();
        let mut items = Vec::new();
        for entry in entries {
            self.conformances.emitted.push((
                entry.target.clone(),
                entry.spec.borrow().id,
                entry.spec_args.clone(),
            ));
            let mut bounds = vec![ResolvedBound::new(
                entry.target.clone(),
                entry.spec.clone(),
                entry.spec_args.clone(),
            )];
            let keys_run = self.with_analyzer(
                &entry.module,
                &entry.substitution,
                AnalysisSite::new(entry.id, entry.span),
                |a| a.expand_bound_set(entry.id, entry.span, &entry.declared_bounds),
            );
            self.diagnostics
                .record_warnings(&entry.module, keys_run.warnings);
            let keys = keys_run.result;
            bounds.extend(self.bound_context_over(&entry.declared_bounds, &keys));
            let owner = Self::conformance_owner(&entry);
            for (function, method_id) in entry.functions.iter().zip(&entry.method_ids) {
                let Some((_, method)) = entry
                    .methods
                    .iter()
                    .find(|(_, method)| method.decl_id == *method_id)
                else {
                    continue;
                };
                let run = self.with_analyzer_in(
                    path,
                    &entry.substitution,
                    &bounds,
                    AnalysisSite::new(function.id, function.span),
                    |analyzer| {
                        analyzer.check_function_body(
                            function,
                            &method.fn_type,
                            method.decl_id,
                            &method.annotations,
                        )
                    },
                );
                if let Some(mut checked) = run.result {
                    checked.conformance_owner = Some(owner.clone());
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
            for pending in &entry.pending {
                let run = self.with_analyzer_in(
                    path,
                    &pending.substitution,
                    &bounds,
                    AnalysisSite::new(pending.id, pending.raw.span),
                    |analyzer| analyzer.check_pending_spec_method(pending),
                );
                if let Some(mut checked) = run.result {
                    checked.conformance_owner = Some(owner.clone());
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
        }
        items
    }

    fn check_primitive_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let entries: Vec<_> = self
            .primitives
            .entries
            .iter()
            .filter(|entry| {
                entry.module == path && !self.primitives.emitted.contains(&entry.target)
            })
            .cloned()
            .collect();
        let mut items = Vec::new();
        for entry in entries {
            self.primitives.emitted.push(entry.target.clone());
            for (function, method_id) in entry.functions.iter().zip(&entry.method_ids) {
                let Some((_, method)) = entry
                    .methods
                    .iter()
                    .find(|(_, method)| method.decl_id == *method_id)
                else {
                    continue;
                };
                let run = self.with_analyzer(
                    path,
                    &entry.substitution,
                    AnalysisSite::new(function.id, function.span),
                    |analyzer| {
                        analyzer.check_function_body(
                            function,
                            &method.fn_type,
                            method.decl_id,
                            &method.annotations,
                        )
                    },
                );
                if let Some(mut checked) = run.result {
                    checked.primitive_target = Some(entry.target.clone());
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
        }
        items
    }

    pub(super) fn emission_module<'a>(
        &self,
        modules: &'a mut CheckedModules,
        path: &[Ident],
    ) -> &'a mut CheckedModule {
        if let Some(index) = modules.iter().position(|(candidate, _)| candidate == path) {
            return &mut modules[index].1;
        }

        let id = self.modules.parsed(path).id;
        modules.push((
            path.to_vec(),
            CheckedModule {
                id,
                items: Vec::new(),
            },
        ));
        &mut modules
            .last_mut()
            .expect("an emitted module was just appended")
            .1
    }

    pub(super) fn drain_pending_declaration_bodies(
        &mut self,
        modules: &mut CheckedModules,
        warnings: &mut TaggedWarnings,
    ) {
        loop {
            let primitive_module = self
                .primitives
                .entries
                .iter()
                .find(|entry| {
                    !self.primitives.emitted.contains(&entry.target)
                        && (!self.roots.is_extern(&entry.module) || entry.monomorphized)
                })
                .map(|entry| entry.module.clone());
            let conformance_module = self
                .conformances
                .entries
                .iter()
                .find(|entry| {
                    !self.conformances.emitted.contains(&(
                        entry.target.clone(),
                        entry.spec.borrow().id,
                        entry.spec_args.clone(),
                    )) && (!self.roots.is_extern(&entry.module)
                        || entry.origin != ConformanceOrigin::Concrete)
                })
                .map(|entry| entry.module.clone());
            let Some((module, primitive)) = primitive_module
                .map(|module| (module, true))
                .or_else(|| conformance_module.map(|module| (module, false)))
            else {
                break;
            };
            let items = if primitive {
                self.check_primitive_bodies(&module, warnings)
            } else {
                self.check_conformance_bodies(&module, warnings)
            };
            if items.is_empty() {
                continue;
            }
            self.emission_module(modules, &module).items.extend(items);
        }
    }

    fn synthesize_gap_items(&mut self, path: &[Ident]) -> Vec<CheckedItem> {
        let mut items = Vec::new();
        for (name, index) in self.modules.index(path).plain_items() {
            if matches!(self.modules.parsed(path).hir.items[index], HirItem::Gap(_)) {
                let key = ItemKey::new(path, &name, &[]);
                let Some(gap) = self.items.gaps.get(&key) else {
                    continue;
                };
                for (fn_name, gap_fn) in &gap.functions {
                    items.push(CheckedItem::ForeignBinding(CheckedForeignBinding {
                        id: gap_fn.decl_id,
                        span: gap_fn.span,
                        ident: fn_name.clone(),
                        r#type: ResolvedType::Function(gap_fn.fn_type.clone()),
                        mangling: ManglingMode::Glued {
                            spec_module_path: gap.module_path.clone(),
                            spec_name: gap.name.clone(),
                            function_name: fn_name.clone(),
                        },
                    }));
                }
                continue;
            }
        }
        items
    }
}
