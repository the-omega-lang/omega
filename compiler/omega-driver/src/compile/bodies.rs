use super::*;
use omega_analyzer::checked::CheckedFunctionDef;
use omega_analyzer::generics::GenericSubstitution;

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
        path: &ModulePath,
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let mut bodies: Vec<CheckedBody> = Vec::new();

        for (name, indices) in self.modules.index(path).item_groups() {
            for index in indices {
                let key = ItemKey::new(path, &name, index, &[]);
                bodies.extend(self.ensure_item_body(&key));
            }
        }

        let mut items = Vec::with_capacity(bodies.len());
        for body in bodies {
            items.push(body.item);
            warnings.extend(body.warnings.into_iter().map(|w| (path.clone(), w)));
        }
        items.extend(self.synthesize_gap_items(path));
        items.extend(self.check_glue_bodies(path, warnings));
        items.extend(self.check_primitive_bodies(path, warnings));
        items.extend(self.check_conformance_bodies(path, warnings));
        items
    }

    fn check_glue_bodies(
        &mut self,
        path: &ModulePath,
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let signatures: Vec<(
            std::rc::Rc<omega_analyzer::resolved_type::ResolvedGap>,
            Vec<(
                omega_hir::HirFunctionDef,
                ResolvedFunctionType,
                omega_analyzer::annotations::ResolvedAnnotations,
            )>,
        )> = self
            .items
            .glues
            .iter()
            .filter(|glue| &glue.module == path)
            .map(|glue| (glue.gap.clone(), glue.functions.clone()))
            .collect();
        let mut items = Vec::new();
        for (gap, functions) in signatures {
            for (function, fn_type, annotations) in functions {
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
                    symbol: SymbolPolicy::mangled(ManglingMode::Glued {
                        spec_module_path: gap.module_path.clone(),
                        spec_name: gap.name.clone(),
                        function_name: function.name.clone(),
                    }),
                    ..annotations
                };
                let run = self.with_analyzer(
                    path,
                    &GenericSubstitution::new(),
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
                        .map(|warning| (path.clone(), warning)),
                );
            }
        }
        items
    }

    fn check_conformance_bodies(
        &mut self,
        path: &ModulePath,
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let entries: Vec<_> = self
            .conformances
            .entries
            .iter()
            .filter(|entry| {
                &entry.module == path
                    && (!self.roots.is_extern(&entry.module)
                        || entry.origin != ConformanceOrigin::Concrete)
                    && !self.conformances.emitted.contains(&(
                        entry.target.clone(),
                        entry.spec.decl_id,
                        entry.spec_args.clone(),
                    ))
            })
            .cloned()
            .collect();
        let mut items = Vec::new();
        for entry in entries {
            self.conformances.emitted.push((
                entry.target.clone(),
                entry.spec.decl_id,
                entry.spec_args.clone(),
            ));
            let (bounds, bound_warnings) = self.conformance_bounds(&entry);
            self.diagnostics
                .record_warnings(&entry.module, bound_warnings);
            let method_ids = entry
                .method_ids
                .iter()
                .chain(entry.pending.iter().map(|pending| &pending.id));
            for method_id in method_ids {
                if let Some(body) = self.conformance_method_body(&entry, &bounds, *method_id) {
                    items.push(CheckedItem::FunctionDefinition(body.function));
                    warnings.extend(body.warnings);
                }
            }
        }
        items
    }

    fn check_primitive_bodies(
        &mut self,
        path: &ModulePath,
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let entries: Vec<_> = self
            .primitives
            .entries
            .iter()
            .filter(|entry| {
                &entry.module == path && !self.primitives.emitted.contains(&entry.target)
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
                        .map(|warning| (path.clone(), warning)),
                );
            }
        }
        items
    }

    pub(super) fn emission_module<'a>(
        &self,
        modules: &'a mut CheckedModules,
        path: &ModulePath,
    ) -> &'a mut CheckedModule {
        if let Some(index) = modules.iter().position(|(candidate, _)| candidate == path) {
            return &mut modules[index].1;
        }

        let id = self.modules.parsed(path).id;
        modules.push((
            path.clone(),
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

    /// Emits every body this compilation owes: the generic instantiations and
    /// pending primitive/conformance bodies checked code produced, and each
    /// concrete member of an instantiated owner that emitted code names. An
    /// instance member nothing names is never checked or emitted, so an owner
    /// type costs only the members a program uses. Checking any of these can
    /// produce more of every kind, so this runs to a fixpoint.
    ///
    /// Generic bodies are emitted by this invocation even when their template
    /// lives in an extern package, under the template's declared module path:
    /// MIR uses that path as part of symbol identity.
    pub(super) fn emit_reachable_bodies(
        &mut self,
        modules: &mut CheckedModules,
        warnings: &mut TaggedWarnings,
    ) {
        let mut emitted_instantiations = 0;
        let mut scanned: IndexMap<ModulePath, usize> = IndexMap::new();
        let mut reached_members: std::collections::HashSet<HirId> = Default::default();
        let mut members: IndexMap<ItemKey, Vec<CheckedFunctionDef>> = IndexMap::new();
        let mut referenced: Vec<HirId> = Vec::new();
        loop {
            while let Some((key, body)) = self
                .items
                .generic_instantiations
                .get_index(emitted_instantiations)
                .map(|(key, body)| (key.clone(), body.clone_of()))
            {
                emitted_instantiations += 1;
                self.emission_module(modules, key.module())
                    .items
                    .push(body.item);
                warnings.extend(
                    body.warnings
                        .into_iter()
                        .map(|warning| (key.module().clone(), warning)),
                );
            }
            self.drain_pending_declaration_bodies(modules, warnings);

            let mut usage = Usage::default();
            for (path, module) in modules.iter() {
                let start = scanned.get(path).copied().unwrap_or(0);
                for item in &module.items[start..] {
                    dead_code::collect_item(item, &mut usage);
                }
                scanned.insert(path.clone(), module.items.len());
            }
            referenced.extend(usage.functions);
            if referenced.is_empty()
                && emitted_instantiations == self.items.generic_instantiations.len()
            {
                break;
            }

            for decl_id in std::mem::take(&mut referenced) {
                if reached_members.contains(&decl_id) {
                    continue;
                }
                let Some(owner) = self.instance_member_owner(decl_id) else {
                    continue;
                };
                reached_members.insert(decl_id);
                let Some(body) = self.ensure_instance_member_body(decl_id) else {
                    continue;
                };
                let mut usage = Usage::default();
                dead_code::collect_item(&body.item, &mut usage);
                referenced.extend(usage.functions);
                let CheckedItem::FunctionDefinition(function) = body.item else {
                    unreachable!("an instance member body is a function definition");
                };
                warnings.extend(
                    body.warnings
                        .into_iter()
                        .map(|warning| (owner.module().clone(), warning)),
                );
                members.entry(owner).or_default().push(function);
            }
        }

        for (owner, functions) in members {
            self.attach_instance_members(modules, &owner, functions);
        }
    }

    /// Adds the members an instantiated owner was found to need to that
    /// owner's emitted item, where its member symbols belong.
    fn attach_instance_members(
        &self,
        modules: &mut CheckedModules,
        owner: &ItemKey,
        functions: Vec<CheckedFunctionDef>,
    ) {
        let declared = match &self.modules.hir(owner.module()).items[owner.disambiguator] {
            HirItem::Struct(s) => s.id,
            HirItem::Enum(e) => e.id,
            HirItem::Union(u) => u.id,
            _ => unreachable!("an instance member's owner is an aggregate"),
        };
        let module = self.emission_module(modules, owner.module());
        let target = module.items.iter_mut().find_map(|item| match item {
            CheckedItem::Struct(s) if s.id == declared && s.generic_args == owner.generic_args => {
                Some(&mut s.functions)
            }
            CheckedItem::Enum(e) if e.id == declared && e.generic_args == owner.generic_args => {
                Some(&mut e.functions)
            }
            CheckedItem::Union(u) if u.id == declared && u.generic_args == owner.generic_args => {
                Some(&mut u.functions)
            }
            _ => None,
        });
        // A member checks only after its owner's instantiation resolved; an
        // owner whose own body failed was reported and emits nothing.
        if let Some(target) = target {
            target.extend(functions);
        }
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
                        entry.spec.decl_id,
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

    fn synthesize_gap_items(&mut self, path: &ModulePath) -> Vec<CheckedItem> {
        let mut items = Vec::new();
        for (name, index) in self.modules.index(path).plain_items() {
            if matches!(self.modules.parsed(path).hir.items[index], HirItem::Gap(_)) {
                let key = ItemKey::new(path, &name, index, &[]);
                let Some(gap) = self.items.gaps.get(&key) else {
                    continue;
                };
                for (fn_name, gap_fn) in &gap.functions {
                    items.push(CheckedItem::ForeignBinding(CheckedForeignBinding {
                        id: gap_fn.decl_id,
                        span: gap_fn.span,
                        ident: fn_name.clone(),
                        r#type: ResolvedType::Function(gap_fn.fn_type.clone()),
                        symbol: SymbolPolicy::mangled(ManglingMode::Glued {
                            spec_module_path: gap.module_path.clone(),
                            spec_name: gap.name.clone(),
                            function_name: fn_name.clone(),
                        }),
                    }));
                }
                continue;
            }
        }
        items
    }
}
