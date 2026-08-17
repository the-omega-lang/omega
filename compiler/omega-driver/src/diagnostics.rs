//! Where per-module findings accumulate, and the one way a throwaway
//! per-item `Analyzer` is ever run.
//!
//! Analysis is item-granular: a module's findings are produced by many short
//! `Analyzer` runs spread across both phases (and, for a generic
//! instantiation, at whatever arbitrary point some use site first triggers
//! it), so they have to be collected somewhere module-wide and drained once
//! at the end rather than returned from any single call.

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

/// Every analysis finding produced so far, bucketed by the module it belongs
/// to.
#[derive(Default)]
pub(crate) struct Diagnostics {
    errors: HashMap<ModulePath, Vec<AnalysisError>>,
    warnings: HashMap<ModulePath, Vec<AnalysisWarning>>,
    /// Field/variant usage recorded from every `Analyzer` run's own
    /// `comp`-evaluated subtrees (see `Analyzer::field_usage`'s doc
    /// comment) -- folded in by every `with_analyzer` call, drained once by
    /// `compile::Driver::compile` and merged with its own post-hoc,
    /// whole-program `crate::dead_code::collect_module` walk before the
    /// final unused-field/never-constructed-variant sweep.
    comp_field_usage: FieldUsage,
}

impl Diagnostics {
    pub fn error(&mut self, module: &[Ident], error: AnalysisError) {
        self.errors.entry(module.to_vec()).or_default().push(error);
    }

    /// Records a finished `Analyzer` run's errors, reporting whether it
    /// produced any -- several callers treat "recorded an error" as "this
    /// item failed" without needing the errors themselves.
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

    /// Drains every error recorded for a module in `scope`, in the
    /// `CompileError` shape `compile` returns on failure.
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

    /// `drain_errors`'s warning counterpart, tagging each warning with the
    /// module it belongs to so the CLI can render it against the right file.
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

    /// Takes every `comp`-evaluation field/variant usage recorded so far,
    /// leaving `FieldUsage::default()` behind -- `compile::Driver::compile`
    /// calls this exactly once, after every module has finished checking,
    /// to merge into its own post-hoc whole-program walk.
    pub fn take_comp_field_usage(&mut self) -> FieldUsage {
        std::mem::take(&mut self.comp_field_usage)
    }
}

/// What one throwaway `Analyzer` run produced besides its own result.
pub(crate) struct AnalyzerRun<R> {
    pub result: R,
    /// Whether the run recorded at least one error. The errors themselves are
    /// already in the sink; this only answers "did this item fail".
    pub failed: bool,
    /// Handed back rather than recorded, because a body check's warnings flow
    /// straight out through `compile`'s own return value.
    pub warnings: Vec<AnalysisWarning>,
}

impl Driver {
    /// Runs `f` against one throwaway `Analyzer` built for a single item,
    /// seeded with `generics` (a generic instantiation's substitution, plus
    /// `Self` where one applies), folding whatever errors it produced into
    /// the sink.
    ///
    /// One `Analyzer` handles exactly one item: nothing is shared between two
    /// of them except this driver, so an item's analysis can never be
    /// polluted by a sibling's state.
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

    /// [`Self::with_analyzer`] for a run whose warnings have no return path
    /// of their own -- a signature is resolved once, memoized, and never
    /// revisited, so its warnings must be captured the one time it runs.
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
