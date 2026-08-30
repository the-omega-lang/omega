use super::*;
use omega_analyzer::generics::GenericSubstitution;

impl Driver {
    /// Extern modules that parsed and indexed. Each is independent: a broken
    /// one is poisoned and skipped while the rest still contribute signatures.
    pub(super) fn collect_extern_signatures(&mut self) -> Vec<ModulePath> {
        let declared = self.roots.extern_modules();
        let mut paths = Vec::with_capacity(declared.len());
        for path in declared {
            match self.parse_module(&path) {
                Ok(_) => paths.push(path),
                Err(error) => {
                    let failure = self.load_failure(&path, error, None);
                    self.diagnostics.fail(Some(&path), failure);
                    self.diagnostics.poison(&path);
                }
            }
        }

        let mut indexed = Vec::with_capacity(paths.len());
        for path in paths {
            if let Err(error) = self.ensure_module_indexed(&path) {
                self.record_module_failure(&path, error);
                continue;
            }
            for (name, index) in self.modules.index(&path).plain_items() {
                let relevant = matches!(
                    self.modules.parsed(&path).hir.items[index],
                    HirItem::Struct(_) | HirItem::Spec(_) | HirItem::Gap(_)
                );
                if !relevant {
                    continue;
                }
                match self.is_generic_template(&path, &name) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        self.record_item_failure(&path, error);
                        continue;
                    }
                }
                let _ = self.ensure_item(&path, &path, &name, &[], ResolveItemOptions::INDIRECT);
            }
            indexed.push(path);
        }
        indexed
    }

    /// Records a failure that makes a whole module unusable and stops
    /// dependent work on it. Independent modules keep going.
    pub(super) fn record_module_failure(&mut self, path: &[Ident], error: ResolveError) {
        self.record_item_failure(path, error);
        self.diagnostics.poison(path);
    }

    /// Records a failure of one item. `ItemFailed` is the marker for a query
    /// whose real reason was already reported where it happened, so recording
    /// it again would make the secondary message the only visible one.
    pub(super) fn record_item_failure(&mut self, path: &[Ident], error: ResolveError) {
        if let ResolveError::ItemFailed { module, .. } = &error
            && self.diagnostics.reported_for(module)
        {
            return;
        }
        let failure = self.load_failure(path, error, None);
        self.diagnostics.fail(Some(path), failure);
    }

    pub(super) fn collect_glue_signatures(&mut self, paths: &[ModulePath]) {
        for path in paths {
            // A poisoned module cannot resolve the gap its glue names, and the
            // reason is already reported; resolving anyway would only repeat
            // that unavailability at every glue block.
            if self.diagnostics.is_poisoned(path) {
                continue;
            }
            let glues: Vec<HirGlueDef> = self
                .modules
                .parsed(path)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Glue(glue) => Some(glue.clone()),
                    _ => None,
                })
                .collect();
            for glue in glues {
                let run = self.with_analyzer(
                    path,
                    &GenericSubstitution::new(),
                    AnalysisSite::new(glue.id, glue.span),
                    |analyzer| {
                        let gap = analyzer.resolve_gap_path(glue.id, glue.span, &glue.gap)?;
                        let functions = glue
                            .functions
                            .iter()
                            .map(|function| {
                                analyzer
                                    .collect_function_signature(function)
                                    .map(|(signature, _)| (function.clone(), signature))
                            })
                            .collect::<Option<Vec<_>>>()?;
                        let mut errors = Vec::new();
                        let mut seen = std::collections::HashMap::new();
                        for (function, _) in &functions {
                            if let Some(previous) =
                                seen.insert(function.name.clone(), function.span)
                            {
                                errors.push(AnalysisError::new(
                                    function.id,
                                    function.span,
                                    AnalysisErrorKind::Redeclaration {
                                        name: function.name.clone(),
                                        previous: Some(previous),
                                    },
                                ));
                            }
                        }
                        for (name, requirement) in &gap.functions {
                            match functions
                                .iter()
                                .find(|(function, _)| function.name == *name)
                            {
                                None => {
                                    errors.push(AnalysisError::new(
                                        glue.id,
                                        glue.span,
                                        AnalysisErrorKind::GlueMissingFunction {
                                            gap: gap.name.clone(),
                                            function: name.clone(),
                                        },
                                    ));
                                }
                                Some((function, actual))
                                    if !Self::same_glue_signature(&requirement.fn_type, actual) =>
                                {
                                    errors.push(AnalysisError::new(
                                        function.id,
                                        function.signature_span,
                                        AnalysisErrorKind::GlueFunctionSignatureMismatch {
                                            gap: gap.name.clone(),
                                            function: name.clone(),
                                        },
                                    ));
                                }
                                Some(_) => {}
                            }
                        }
                        for (function, _) in &functions {
                            if !gap.functions.iter().any(|(name, _)| *name == function.name) {
                                errors.push(AnalysisError::new(
                                    function.id,
                                    function.name_span,
                                    AnalysisErrorKind::GlueExtraFunction {
                                        gap: gap.name.clone(),
                                        function: function.name.clone(),
                                    },
                                ));
                            }
                        }
                        Some((gap, functions, errors))
                    },
                );
                self.diagnostics.record_warnings(path, run.warnings);
                if run.failed {
                    continue;
                }
                let Some((gap, functions, errors)) = run.result else {
                    continue;
                };
                if self.diagnostics.record_errors(path, errors) {
                    continue;
                }
                self.items.glues.push(GlueSignature {
                    module: path.clone(),
                    span: glue.span,
                    gap,
                    functions,
                });
            }
        }
    }

    pub(super) fn same_glue_signature(
        expected: &ResolvedFunctionType,
        actual: &ResolvedFunctionType,
    ) -> bool {
        expected.is_variadic == actual.is_variadic
            && expected.self_mode == actual.self_mode
            && expected.return_type == actual.return_type
            && expected.params == actual.params
    }

    pub(super) fn sweep_gaps(&self) -> (TaggedWarnings, Vec<CompileError>) {
        let mut warnings = TaggedWarnings::new();
        let mut errors = Vec::new();
        for (key, gap) in &self.items.gaps {
            // A glue block binds no name, so the only honest way to name the
            // conflicting implementations is to label the blocks themselves.
            let glues: Vec<omega_diagnostics::SourceSpan> = self
                .items
                .glues
                .iter()
                .filter(|glue| glue.gap.id == gap.id)
                .filter_map(|glue| self.site(&glue.module, glue.span))
                .collect();
            match glues.as_slice() {
                [] => warnings.push((
                    key.module.clone(),
                    AnalysisWarning::new(
                        gap.id,
                        gap.span,
                        AnalysisWarningKind::UnfilledGap {
                            gap: gap.name.clone(),
                            functions: gap.functions.iter().map(|(name, _)| name.clone()).collect(),
                        },
                    ),
                )),
                [_] => {}
                _ => errors.push(CompileError::Analysis {
                    module: key.module.clone(),
                    errors: vec![AnalysisError::new(
                        gap.id,
                        gap.span,
                        AnalysisErrorKind::MultipleGluesForGap {
                            gap: gap.name.clone(),
                            glues,
                        },
                    )],
                }),
            }
        }
        (warnings, errors)
    }

    pub(super) fn collect_signatures(&mut self, local: &[ModulePath], entry: &[Ident]) {
        // Import processing comes first and completely: a module whose
        // bindings are broken cannot be read reliably, so every local module's
        // import targets are answered before any of them resolves a name.
        // Reporting the failure here is also what keeps it to one diagnostic,
        // at the import, instead of one per use that reaches through it. A
        // module with broken bindings is poisoned rather than aborting the
        // phase, so unrelated modules still report their own errors.
        for path in local {
            if let Err(error) = self.ensure_module_indexed(path) {
                self.record_module_failure(path, error);
                continue;
            }
            if self.validate_imports(path) {
                self.diagnostics.poison(path);
            }
        }

        for path in local {
            if self.diagnostics.is_poisoned(path) {
                continue;
            }
            // A function's *effective* generics, not its written ones: a
            // static-spec parameter (`f(x: spec S)`) normalizes into a
            // generic bound, and that bound is the only place the import of
            // `S` is used. The eager item sweep below skips the resulting
            // lazy template entirely, so nothing else would ever record it.
            let functions: Vec<omega_hir::HirFunctionDef> = self
                .modules
                .parsed(path)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::FunctionDefinition(f) => Some(f.clone()),
                    _ => None,
                })
                .collect();
            for f in &functions {
                match self.normalized_function(path, f) {
                    Ok(normalized) => self.mark_bound_type_imports(path, &normalized.generics),
                    Err(error) => self.record_item_failure(path, error),
                }
            }

            let declared_generics: Vec<Vec<HirGenericParam>> = self
                .modules
                .parsed(path)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Struct(s) => Some(s.generics.clone()),
                    HirItem::Enum(e) => Some(e.generics.clone()),
                    HirItem::Union(u) => Some(u.generics.clone()),
                    HirItem::Spec(sp) => Some(sp.generics.clone()),
                    HirItem::Primitive(p) => Some(p.generics.clone()),
                    _ => None,
                })
                .collect();
            for generics in &declared_generics {
                self.mark_bound_type_imports(path, generics);
            }

            self.validate_aliases(path);

            for (name, _) in self.modules.index(path).plain_items() {
                match self.is_generic_template(path, &name) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        self.record_item_failure(path, error);
                        continue;
                    }
                }
                let _ = self.ensure_item(path, path, &name, &[], ResolveItemOptions::INDIRECT);
            }

            for (name, indices) in self.modules.index(path).overloads.clone() {
                let signatures: Result<Vec<ResolvedFunctionType>, ResolveError> = indices
                    .iter()
                    .map(|&i| self.ensure_overload_signature(path, i))
                    .collect();
                match signatures {
                    Ok(signatures) => {
                        self.check_overload_duplicates(path, &name, &indices, &signatures)
                    }
                    Err(error) => self.record_item_failure(path, error),
                }
            }
        }

        self.check_main_signature(entry);
    }

    // A root-module `main` is enforced to be exactly `main() => void` or
    // `main() => never` -- command-line arguments and process exit codes
    // are platform-dependent, so `main` stays a portable, argument-less
    // entry point. See `docs/language/foreign-function-interface.md`.
    fn check_main_signature(&mut self, entry: &[Ident]) {
        // A namespace-only root directory (a library package with no root
        // `.omg` file of its own) was never parsed/indexed and cannot
        // declare a `main` at all.
        let Some(index) = self.modules.get(entry).and_then(|m| m.index.as_ref()) else {
            return;
        };
        let name = Ident("main".to_owned());
        let indices: Vec<usize> = match index.overloads.get(&name) {
            Some(indices) => indices.clone(),
            None => match index.items.get(&name) {
                Some(&i) => vec![i],
                None => return,
            },
        };
        let is_overloaded = index.overloads.contains_key(&name);

        for index in indices {
            let hir = self.modules.hir(entry);
            let HirItem::FunctionDefinition(f) = &hir.items[index] else {
                continue;
            };
            if !f.generics.is_empty() {
                continue;
            }
            let (id, span) = (f.id, f.signature_span);

            let fn_type = if is_overloaded {
                self.ensure_overload_signature(entry, index).ok()
            } else {
                match self.ensure_item(entry, entry, &name, &[], ResolveItemOptions::INDIRECT) {
                    Ok(ResolvedItem::Value {
                        r#type: ResolvedType::Function(fn_type),
                        ..
                    }) => Some(fn_type),
                    _ => None,
                }
            };

            let Some(fn_type) = fn_type else { continue };
            let valid = fn_type.params.is_empty()
                && matches!(
                    *fn_type.return_type,
                    ResolvedType::Void | ResolvedType::Never
                );
            if !valid {
                self.diagnostics.error(
                    entry,
                    AnalysisError::new(id, span, AnalysisErrorKind::InvalidMainSignature),
                );
            }
        }
    }
}
