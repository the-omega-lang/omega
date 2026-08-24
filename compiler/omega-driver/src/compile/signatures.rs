use super::*;

impl Driver {
    pub(super) fn collect_extern_signatures(
        &mut self,
    ) -> Result<Vec<ModulePath>, Vec<CompileError>> {
        let paths = self.roots.extern_modules();
        for path in &paths {
            if let Err(error) = self.parse_module(path) {
                return Err(vec![self.load_failure(path, error, None)]);
            }
        }
        for path in &paths {
            self.ensure_module_indexed(path).map_err(fatal)?;
            for (name, index) in self.modules.index(path).plain_items() {
                let relevant = matches!(
                    self.modules.parsed(path).hir.items[index],
                    HirItem::Struct(_) | HirItem::Spec(_) | HirItem::Gap(_)
                );
                if !relevant || self.is_generic_template(path, &name).map_err(fatal)? {
                    continue;
                }
                let _ = self.ensure_item(path, path, &name, &[], ResolveItemOptions::INDIRECT);
            }
        }
        Ok(paths)
    }

    pub(super) fn collect_glue_signatures(&mut self, paths: &[ModulePath]) {
        for path in paths {
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
                    &[],
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
                    id: glue.id,
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
            let glues: Vec<Ident> = self
                .items
                .glues
                .iter()
                .filter(|glue| glue.gap.id == gap.id)
                .map(|glue| {
                    Ident(format!(
                        "{}#{}",
                        glue.module
                            .iter()
                            .map(Ident::as_ref)
                            .collect::<Vec<_>>()
                            .join("::"),
                        glue.id.local
                    ))
                })
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

    pub(super) fn collect_signatures(
        &mut self,
        local: &[ModulePath],
        entry: &[Ident],
    ) -> Result<(), Vec<CompileError>> {
        for path in local {
            self.ensure_module_indexed(path).map_err(fatal)?;

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
                let normalized = self.normalized_function(path, f).map_err(fatal)?;
                self.mark_bound_type_imports(path, &normalized.generics);
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
                if self.is_generic_template(path, &name).map_err(fatal)? {
                    continue;
                }
                let _ = self.ensure_item(path, path, &name, &[], ResolveItemOptions::INDIRECT);
            }

            for (name, indices) in self.modules.index(path).overloads.clone() {
                let signatures: Vec<ResolvedFunctionType> = indices
                    .iter()
                    .map(|&i| self.ensure_overload_signature(path, i))
                    .collect::<Result<_, _>>()
                    .map_err(fatal)?;
                self.check_overload_duplicates(path, &name, &indices, &signatures);
            }
        }

        self.check_main_signature(entry);

        let errors = self.diagnostics.drain_errors(local);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
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
