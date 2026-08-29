use super::*;

impl Driver {
    pub(crate) fn visibility_allows(
        visibility: Visibility,
        declaring: &[Ident],
        accessor: &[Ident],
    ) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Shared => declaring.first() == accessor.first(),
            Visibility::Hidden => declaring == accessor,
        }
    }

    pub(crate) fn declared_visibility(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Option<Visibility> {
        let index = self.local_item_index(module_path, name).ok()?;
        self.modules
            .parsed(module_path)
            .hir
            .items
            .get(index)
            .map(item_visibility)
    }

    fn gate_visibility(
        item: ResolvedItem,
        visibility: Visibility,
        key: &ItemKey,
        accessor: &[Ident],
        bypass: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        if bypass || Self::visibility_allows(visibility, &key.module, accessor) {
            Ok(item)
        } else {
            Err(ResolveError::NotVisible {
                module: key.module.clone(),
                item: key.name.clone(),
            })
        }
    }

    fn in_progress_result(
        &self,
        key: &ItemKey,
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        if !indirect {
            return Err(ResolveError::RecursiveTypeWithoutIndirection {
                module: key.module.clone(),
                item: key.name.clone(),
            });
        }
        match self.items.cells.resolved_type(key) {
            Some(r#type) => Ok(ResolvedItem::Type(r#type)),
            None => Err(ResolveError::Cycle(self.items.cycle_path(key))),
        }
    }

    pub(crate) fn ensure_item(
        &mut self,
        accessor_module_path: &[Ident],
        module_path: &[Ident],
        name: &Ident,
        type_args: &[ResolvedType],
        options: ResolveItemOptions,
    ) -> Result<ResolvedItem, ResolveError> {
        // `type_args` must be padded with defaults before `ItemKey` is built:
        // its equality is structural, so two call sites meaning the same
        // effective types must produce the identical key to share one
        // instantiation.
        let index = self.local_item_index(module_path, name)?;
        let generic_params = self.item_generics(module_path, name)?;
        let type_args =
            self.pad_generic_defaults(module_path, name, index, &generic_params, type_args)?;
        let key = ItemKey::new(module_path, name, &type_args);

        match self.items.state(&key) {
            Some(ItemQueryState::Resolved(entry)) => {
                return Self::gate_visibility(
                    entry.item.clone(),
                    entry.visibility,
                    &key,
                    accessor_module_path,
                    options.bypasses_visibility(),
                );
            }
            // Secondary by construction: the query that failed kept its own
            // reason, which was already delivered where it happened.
            Some(ItemQueryState::Failed(_)) => return Err(key.failed()),
            Some(ItemQueryState::InProgress) => {
                return self.in_progress_result(&key, options.allows_indirection());
            }
            None => {}
        }

        if generic_params.iter().any(|g| !g.bounds.is_empty()) {
            self.check_item_generic_bounds(&key, index, &generic_params, &type_args)?;
        }

        let visibility = self
            .declared_visibility(module_path, name)
            .expect("just indexed by local_item_index");
        let generics: Vec<Ident> = generic_params.iter().map(|g| g.ident.clone()).collect();

        self.items.begin(&key);
        let result = self.compute_item(&key, index, &generics);
        self.items.finish(&key, visibility, result.as_ref());

        // An instantiation's body is checked right here, once its signature
        // is resolved -- preserves the invariant that a recursive call never
        // hits `InProgress`, for a generic call `compile`'s static sweep
        // could never enumerate.
        if result.is_ok() && key.is_instantiation() {
            self.check_generic_instantiation_body(&key, index);
        }

        Self::gate_visibility(
            result?,
            visibility,
            &key,
            accessor_module_path,
            options.bypasses_visibility(),
        )
    }

    pub(crate) fn pad_generic_defaults(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        index: usize,
        generic_params: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Result<Vec<ResolvedType>, ResolveError> {
        if type_args.len() >= generic_params.len() {
            if type_args.len() > generic_params.len() {
                return Err(ResolveError::GenericArgCountMismatch {
                    module: module_path.to_vec(),
                    item: name.clone(),
                    expected: generic_params.len(),
                    found: type_args.len(),
                });
            }
            return Ok(type_args.to_vec());
        }

        let hir = self.modules.hir(module_path);
        let owner = item_site(&hir.items[index]);
        let mut padded = type_args.to_vec();
        for param in &generic_params[type_args.len()..] {
            let Some(default) = &param.default else {
                return Err(ResolveError::GenericArgCountMismatch {
                    module: module_path.to_vec(),
                    item: name.clone(),
                    expected: generic_params.len(),
                    found: type_args.len(),
                });
            };
            let substitution: Vec<(Ident, ResolvedType)> = generic_params
                .iter()
                .map(|g| g.ident.clone())
                .zip(padded.iter().cloned())
                .collect();
            let default = default.clone();
            let run = self.with_analyzer(module_path, &[], owner, |analyzer| {
                analyzer.resolve_under_substitution(owner.id, owner.span, &default, &substitution)
            });
            match (run.failed, run.result) {
                (false, Some(resolved)) => padded.push(resolved),
                _ => {
                    return Err(ResolveError::ItemFailed {
                        module: module_path.to_vec(),
                        item: name.clone(),
                    });
                }
            }
        }
        Ok(padded)
    }

    pub(crate) fn check_generic_bounds(
        &mut self,
        module: &[Ident],
        owner: AnalysisSite,
        generic_params: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Option<Result<Vec<ResolvedBound>, ResolveError>> {
        let substitution: Vec<(Ident, ResolvedType)> = generic_params
            .iter()
            .map(|g| g.ident.clone())
            .zip(type_args.iter().cloned())
            .collect();

        let mut declared = Vec::new();
        for (param, concrete) in generic_params.iter().zip(type_args) {
            let bounds = match omega_analyzer::aliases::expand_bounds(self, module, &param.bounds) {
                Ok(bounds) => bounds,
                Err(error) => return Some(Err(error)),
            };
            for bound in &bounds {
                let run = self.with_analyzer(module, &substitution, owner, |analyzer| {
                    analyzer.check_generic_bound(owner.id, owner.span, bound, concrete)
                });
                if run.failed {
                    return None;
                }
                match run.result {
                    Some(Ok((spec, spec_args))) => {
                        declared.push(ResolvedBound::new(concrete.clone(), spec, spec_args));
                    }
                    Some(Err((spec, missing))) => {
                        return Some(Err(ResolveError::SpecNotImplemented {
                            type_name: concrete.to_string(),
                            spec,
                            missing,
                        }));
                    }
                    None => {}
                }
            }
        }
        Some(Ok(declared))
    }

    fn check_item_generic_bounds(
        &mut self,
        key: &ItemKey,
        index: usize,
        generic_params: &[HirGenericParam],
        type_args: &[ResolvedType],
    ) -> Result<(), ResolveError> {
        let hir = self.modules.hir(&key.module);
        let owner = item_site(&hir.items[index]);
        let declared =
            match self.check_generic_bounds(&key.module, owner, generic_params, type_args) {
                Some(Ok(declared)) => declared,
                Some(Err(error)) => return Err(error),
                None => return Err(key.failed()),
            };
        self.items.declared_bounds.insert(key.clone(), declared);
        Ok(())
    }

    fn compute_item(
        &mut self,
        key: &ItemKey,
        index: usize,
        generics: &[Ident],
    ) -> Result<ResolvedItem, ResolveError> {
        let hir = self.modules.hir(&key.module);
        let item = &hir.items[index];
        let module = &key.module;
        let substitution: Vec<(Ident, ResolvedType)> = generics
            .iter()
            .cloned()
            .zip(key.type_args.iter().cloned())
            .collect();

        let resolved = match item {
            HirItem::Declaration { decl, .. } => self
                .analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(decl.id, decl.span),
                    |a| a.analyze_declaration(decl, Storage::Global, DeclarationPolicy::Unique),
                )
                .map(|c| ResolvedItem::Value {
                    r#type: c.r#type,
                    storage: Storage::Global,
                    decl_id: c.id,
                    mutable: c.mutable,
                }),

            HirItem::DeclarationWithInit { decl, value, .. } => self
                .analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(decl.id, decl.span),
                    |a| a.analyze_global_declaration_with_init(decl, value),
                )
                .map(|c| {
                    if let Some(v) = c.initial_value {
                        self.items.global_initial_values.insert(c.id, v);
                    }
                    ResolvedItem::Value {
                        r#type: c.r#type,
                        storage: Storage::Global,
                        decl_id: c.id,
                        mutable: c.mutable,
                    }
                }),

            // A top-level binding, `comp` or not -- evaluated right here
            // during signature resolution, since `comp <expr>` interprets
            // eagerly as part of ordinary expression analysis. The new
            // reentrancy this opens is guarded by `ensure_item_body`'s own
            // cycle guard.
            //
            // `w.comp` decides which of two things this binding is: a
            // `comp` binding has no storage, so its value lives only in
            // `ItemQueries::comp_values`, substituted at every use; a
            // non-`comp` binding gets real `Storage::Global` storage,
            // like `HirItem::Declaration` above.
            HirItem::Walrus { walrus: w, .. } if w.comp => self
                .analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(w.id, w.span),
                    |a| a.analyze_comp_declaration(w),
                )
                .map(|(r#type, value)| {
                    self.items.comp_values.insert(w.id, value);
                    ResolvedItem::Value {
                        r#type,
                        storage: Storage::Comp,
                        decl_id: w.id,
                        mutable: false,
                    }
                }),
            HirItem::Walrus { walrus: w, .. } => self
                .analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(w.id, w.span),
                    |a| a.analyze_global_walrus(w),
                )
                .map(|c| {
                    if let Some(value) = c.initial_value {
                        self.items.global_initial_values.insert(c.id, value);
                    }
                    ResolvedItem::Value {
                        r#type: c.r#type,
                        storage: Storage::Global,
                        decl_id: c.id,
                        mutable: c.mutable,
                    }
                }),

            HirItem::ForeignBinding(binding) => self
                .analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(binding.id, binding.span),
                    |a| a.analyze_foreign_binding(binding),
                )
                .map(|(r#type, annotations)| {
                    self.items
                        .function_annotations
                        .insert(binding.id, annotations);
                    let storage = match r#type {
                        ResolvedType::Function(_) => Storage::Function,
                        _ => Storage::Global,
                    };
                    ResolvedItem::Value {
                        r#type,
                        storage,
                        decl_id: binding.id,
                        mutable: false,
                    }
                }),

            HirItem::ForeignFunction(f) => {
                let id = self.items.identity_for(key, f.id);
                self.analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(f.id, f.span),
                    |a| a.collect_foreign_function_signature(f),
                )
                .map(|(fn_type, annotations)| {
                    self.items.function_annotations.insert(id, annotations);
                    ResolvedItem::Value {
                        r#type: ResolvedType::Function(fn_type),
                        storage: Storage::Function,
                        decl_id: id,
                        mutable: false,
                    }
                })
            }

            HirItem::FunctionDefinition(f) => {
                let id = self.items.identity_for(key, f.id);
                self.analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(f.id, f.span),
                    |a| a.collect_function_signature(f),
                )
                .map(|(fn_type, annotations)| {
                    self.items.function_annotations.insert(id, annotations);
                    ResolvedItem::Value {
                        r#type: ResolvedType::Function(fn_type),
                        storage: Storage::Function,
                        decl_id: id,
                        mutable: false,
                    }
                })
            }

            HirItem::Struct(s) => {
                let id = self.items.identity_for(key, s.id);
                let cell = self.items.cells.struct_cell(key, id);
                let method_ids = self
                    .items
                    .method_identities(key, s.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Struct(cell.clone());
                self.compute_aggregate(
                    key,
                    AnalysisSite::new(s.id, s.span),
                    &substitution,
                    self_type,
                    method_ids,
                    |a, ids| a.signature_of_struct(s, &cell, ids),
                )
            }

            HirItem::Enum(e) => {
                let id = self.items.identity_for(key, e.id);
                let cell = self.items.cells.enum_cell(key, id);
                let method_ids = self
                    .items
                    .method_identities(key, e.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Enum {
                    cell: cell.clone(),
                    variant: None,
                };
                self.compute_aggregate(
                    key,
                    AnalysisSite::new(e.id, e.span),
                    &substitution,
                    self_type,
                    method_ids,
                    |a, ids| a.signature_of_enum(e, &cell, ids),
                )
            }

            HirItem::Union(u) => {
                let id = self.items.identity_for(key, u.id);
                let cell = self.items.cells.union_cell(key, id);
                let method_ids = self
                    .items
                    .method_identities(key, u.functions.iter().map(|f| f.id));
                let self_type = ResolvedType::Union(cell.clone());
                self.compute_aggregate(
                    key,
                    AnalysisSite::new(u.id, u.span),
                    &substitution,
                    self_type,
                    method_ids,
                    |a, ids| a.signature_of_union(u, &cell, ids),
                )
            }

            HirItem::Gap(gap) => {
                let id = self.items.identity_for(key, gap.id);
                self.analyze(
                    module,
                    &substitution,
                    AnalysisSite::new(gap.id, gap.span),
                    |a| a.signature_of_gap(gap),
                )
                .map(|mut gap| {
                    gap.id = id;
                    let gap = Rc::new(gap);
                    self.items.gaps.insert(key.clone(), gap.clone());
                    ResolvedItem::Gap(gap)
                })
            }

            HirItem::Spec(_) => {
                let absolute: Vec<Ident> =
                    module.iter().cloned().chain([key.name.clone()]).collect();
                self.resolve_spec_declaration(&absolute)?
                    .map(|cell| ResolvedItem::Type(ResolvedType::Spec(cell)))
            }

            HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) => {
                unreachable!("unnamed blocks have no item key")
            }
            HirItem::Import(_) => unreachable!("imports are never indexed into a module's items"),
            HirItem::Alias(_) => {
                unreachable!("aliases are indexed separately and never become an item key")
            }
        };

        resolved.ok_or_else(|| key.failed())
    }

    fn compute_aggregate(
        &mut self,
        key: &ItemKey,
        owner: AnalysisSite,
        substitution: &[(Ident, ResolvedType)],
        self_type: ResolvedType,
        method_ids: Vec<HirId>,
        signature: impl FnOnce(&mut Analyzer, &[HirId]) -> Option<()>,
    ) -> Option<ResolvedItem> {
        let mut substitution = substitution.to_vec();
        substitution.push((Ident("Self".to_string()), self_type.clone()));

        self.analyze(&key.module, &substitution, owner, |analyzer| {
            signature(analyzer, &method_ids)
        })?;
        Some(ResolvedItem::Type(self_type))
    }

    pub(crate) fn resolve_spec_declaration(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError> {
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        let key: SpecKey = (module_path.to_vec(), name.clone());
        match self.items.spec_states.get(&key) {
            Some(SpecQueryState::Resolved(cell)) => return Ok(Some(cell.clone())),
            Some(SpecQueryState::Failed(_)) => {
                return Err(ResolveError::ItemFailed {
                    module: key.0,
                    item: key.1,
                });
            }
            Some(SpecQueryState::InProgress) => {
                return Err(ResolveError::SpecDependencyCycle {
                    module: key.0,
                    spec: key.1,
                });
            }
            None => {}
        }

        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let HirItem::Spec(sp) = &self.modules.parsed(module_path).hir.items[index] else {
            return Ok(None);
        };
        let sp = sp.clone();

        self.items.begin_spec(&key);
        let run = self.with_analyzer(
            module_path,
            &[],
            AnalysisSite::new(sp.id, sp.span),
            |analyzer| analyzer.resolve_spec_functions(&sp),
        );
        self.diagnostics.record_warnings(module_path, run.warnings);

        if run.failed {
            self.items.finish_spec(&key, Err(QueryFailure::Reported));
            return Err(ResolveError::ItemFailed {
                module: key.0,
                item: key.1,
            });
        }
        let (functions, annotations) = run.result;
        let is_object_safe = functions
            .iter()
            .all(|(_, raw)| !matches!(raw.return_type, Type::SpecStatic(_)));
        let cell = Rc::new(RefCell::new(ResolvedSpecType {
            id: sp.id,
            name: sp.name.clone(),
            visibility: sp.visibility,
            generics: sp.generics.iter().map(|g| g.ident.clone()).collect(),
            module_path: module_path.to_vec(),
            type_args: vec![],
            is_object_safe,
            functions,
            suppress: annotations.suppress,
        }));
        self.items.finish_spec(&key, Ok(cell.clone()));
        Ok(Some(cell))
    }
}
