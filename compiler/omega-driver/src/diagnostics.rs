
use crate::error::CompileError;
use crate::{Driver, ModulePath};
use omega_analyzer::analysis::Analyzer;
use omega_analyzer::dead_code::FieldUsage;
use omega_analyzer::error::{AnalysisError, AnalysisWarning};
use omega_analyzer::resolved_type::ResolvedType;
use omega_diagnostics::Span;
use omega_hir::HirId;
use omega_parser::prelude::Ident;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Diagnostics {
    errors: HashMap<ModulePath, Vec<AnalysisError>>,
    warnings: HashMap<ModulePath, Vec<AnalysisWarning>>,
    comp_field_usage: FieldUsage,
}

impl Diagnostics {
    pub fn error(&mut self, module: &[Ident], error: AnalysisError) {
        self.errors.entry(module.to_vec()).or_default().push(error);
    }

    pub fn record_errors(&mut self, module: &[Ident], errors: Vec<AnalysisError>) -> bool {
        if errors.is_empty() {
            return false;
        }
        self.errors
            .entry(module.to_vec())
            .or_default()
            .extend(errors);
        true
    }

    pub fn record_warnings(&mut self, module: &[Ident], warnings: Vec<AnalysisWarning>) {
        if !warnings.is_empty() {
            self.warnings
                .entry(module.to_vec())
                .or_default()
                .extend(warnings);
        }
    }

    pub fn drain_errors(&mut self, scope: &[ModulePath]) -> Vec<CompileError> {
        scope
            .iter()
            .filter_map(|path| {
                let errors = self.errors.remove(path)?;
                (!errors.is_empty()).then(|| CompileError::Analysis {
                    module: path.clone(),
                    errors,
                })
            })
            .collect()
    }

    pub fn drain_warnings(&mut self, scope: &[ModulePath]) -> Vec<(ModulePath, AnalysisWarning)> {
        scope
            .iter()
            .flat_map(|path| {
                self.warnings
                    .remove(path)
                    .into_iter()
                    .flatten()
                    .map(move |w| (path.clone(), w))
            })
            .collect()
    }

    pub fn take_comp_field_usage(&mut self) -> FieldUsage {
        std::mem::take(&mut self.comp_field_usage)
    }
}

pub(crate) struct AnalyzerRun<R> {
    pub result: R,
    pub failed: bool,
    pub warnings: Vec<AnalysisWarning>,
}

impl Driver {
    pub(crate) fn with_analyzer<R>(
        &mut self,
        module: &[Ident],
        generics: &[(Ident, ResolvedType)],
        owner: (HirId, Span),
        f: impl FnOnce(&mut Analyzer) -> R,
    ) -> AnalyzerRun<R> {
        self.with_analyzer_in(module, generics, &[], owner, f)
    }

    pub(crate) fn with_analyzer_in<R>(
        &mut self,
        module: &[Ident],
        generics: &[(Ident, ResolvedType)],
        bounds: &[omega_analyzer::resolved_type::ResolvedBound],
        owner: (HirId, Span),
        f: impl FnOnce(&mut Analyzer) -> R,
    ) -> AnalyzerRun<R> {
        let target = self.target;
        let mut analyzer = Analyzer::new_in(self, module.to_vec(), generics, bounds, owner, target);
        let result = f(&mut analyzer);
        let (errors, warnings, field_usage) = analyzer.finish();
        let failed = self.diagnostics.record_errors(module, errors);
        self.diagnostics.comp_field_usage.merge(field_usage);
        AnalyzerRun {
            result,
            failed,
            warnings,
        }
    }

    pub(crate) fn analyze<R>(
        &mut self,
        module: &[Ident],
        generics: &[(Ident, ResolvedType)],
        owner: (HirId, Span),
        f: impl FnOnce(&mut Analyzer) -> R,
    ) -> R {
        let run = self.with_analyzer(module, generics, owner, f);
        self.diagnostics.record_warnings(module, run.warnings);
        run.result
    }
}
