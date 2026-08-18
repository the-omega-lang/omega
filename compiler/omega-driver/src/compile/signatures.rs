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
            && expected.params.len() == actual.params.len()
            && expected
                .params
                .iter()
                .zip(&actual.params)
                .all(|((_, expected), (_, actual))| expected == actual)
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
    ) -> Result<(), Vec<CompileError>> {
        for path in local {
            self.ensure_module_indexed(path).map_err(fatal)?;

            let generic_bounds: Vec<(Vec<Ident>, Vec<HirGenericParam>)> = self
                .modules
                .parsed(path)
                .hir
                .items
                .iter()
                .filter_map(|item| {
                    let generics = match item {
                        HirItem::FunctionDefinition(f) => Some(&f.generics),
                        HirItem::Struct(s) => Some(&s.generics),
                        HirItem::Enum(e) => Some(&e.generics),
                        HirItem::Union(u) => Some(&u.generics),
                        HirItem::Spec(sp) => Some(&sp.generics),
                        HirItem::Primitive(p) => Some(&p.generics),
                        _ => None,
                    };
                    Some((path.clone(), generics?.clone()))
                })
                .collect();
            for (module, generics) in &generic_bounds {
                self.mark_bound_type_imports(module, generics);
            }

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

        let errors = self.diagnostics.drain_errors(local);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
