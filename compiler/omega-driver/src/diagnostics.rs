use crate::error::CompileError;
use crate::{Driver, ModulePath};
use omega_analyzer::analysis::{AnalysisMode, AnalysisSite, Analyzer};
use omega_analyzer::dead_code::FieldUsage;
use omega_analyzer::error::{AnalysisError, AnalysisWarning};
use omega_analyzer::resolved_type::ResolvedType;
use omega_diagnostics::Span;
use omega_hir::HirId;
use omega_parser::prelude::Ident;
use std::collections::{HashMap, HashSet};

/// The frontend's finding sink. Every phase records into it at the unit that
/// failed and keeps going, so one broken module cannot hide the diagnostics of
/// the modules around it. Nothing here decides when to stop compiling; that is
/// the single barrier in `Driver::compile`.
#[derive(Default)]
pub(crate) struct Diagnostics {
    errors: HashMap<ModulePath, Vec<AnalysisError>>,
    warnings: HashMap<ModulePath, Vec<AnalysisWarning>>,
    /// Parse/macro/resolve/package failures, in discovery order, tagged with
    /// the module they belong to when they have one.
    failures: Vec<(Option<ModulePath>, CompileError)>,
    /// Modules whose prerequisites are unavailable. Work that depends on one
    /// is skipped rather than run against fabricated data.
    poisoned: HashSet<ModulePath>,
    /// Findings already recorded, so a query re-run by a later phase reports
    /// the same fault once. Identity is the node, its span, and the rendered
    /// claim -- two of those are the same finding, not two findings.
    reported: HashSet<(ModulePath, HirId, Span, String)>,
    comp_field_usage: FieldUsage,
}

impl Diagnostics {
    pub fn error(&mut self, module: &[Ident], error: AnalysisError) {
        if !self.is_new(module, &error) {
            return;
        }
        self.errors.entry(module.to_vec()).or_default().push(error);
    }

    fn is_new(&mut self, module: &[Ident], error: &AnalysisError) -> bool {
        self.reported.insert((
            module.to_vec(),
            error.node_id,
            error.span,
            error.kind.to_string(),
        ))
    }

    /// Records a non-analysis frontend failure. A module reports at most one:
    /// a second load attempt returns the same unavailability, not a new fault.
    pub fn fail(&mut self, module: Option<&[Ident]>, error: CompileError) {
        let module = module.map(<[Ident]>::to_vec);
        if let Some(module) = &module
            && self
                .failures
                .iter()
                .any(|(owner, _)| owner.as_ref() == Some(module))
        {
            return;
        }
        self.failures.push((module, error));
    }

    /// Whether anything has already been recorded against `module`. A
    /// secondary "already failed" marker is only worth printing when its
    /// module produced no finding of its own.
    pub fn reported_for(&self, module: &[Ident]) -> bool {
        self.errors
            .get(module)
            .is_some_and(|errors| !errors.is_empty())
            || self
                .failures
                .iter()
                .any(|(owner, _)| owner.as_deref() == Some(module))
    }

    pub fn poison(&mut self, module: &[Ident]) {
        self.poisoned.insert(module.to_vec());
    }

    pub fn is_poisoned(&self, module: &[Ident]) -> bool {
        self.poisoned.contains(module)
    }

    /// Every recorded error, module-ordered over `scope` with each module's
    /// load failure ahead of its analysis errors, then anything left over in
    /// discovery order. Deterministic so exact-output tests stay reliable.
    pub fn drain(&mut self, scope: &[ModulePath]) -> Vec<CompileError> {
        let mut out = Vec::new();
        for path in scope {
            out.extend(self.take_failures(|owner| owner == Some(path)));
            if let Some(errors) = self.errors.remove(path)
                && !errors.is_empty()
            {
                out.push(CompileError::Analysis {
                    module: path.clone(),
                    errors,
                });
            }
        }
        out.extend(self.take_failures(|_| true));
        // `errors` is a hash map, so anything recorded outside `scope` has to
        // be ordered explicitly or the output would vary between runs.
        let mut leftover: Vec<ModulePath> = self.errors.keys().cloned().collect();
        leftover.sort_by(|a, b| a.iter().map(Ident::as_ref).cmp(b.iter().map(Ident::as_ref)));
        for path in leftover {
            let errors = self.errors.remove(&path).unwrap_or_default();
            if !errors.is_empty() {
                out.push(CompileError::Analysis {
                    module: path,
                    errors,
                });
            }
        }
        out
    }

    fn take_failures(&mut self, keep: impl Fn(Option<&ModulePath>) -> bool) -> Vec<CompileError> {
        let mut taken = Vec::new();
        let mut remaining = Vec::new();
        for (owner, error) in std::mem::take(&mut self.failures) {
            if keep(owner.as_ref()) {
                taken.push(error);
            } else {
                remaining.push((owner, error));
            }
        }
        self.failures = remaining;
        taken
    }

    /// Returns whether the unit that produced these findings failed, which
    /// stays true even when every finding was already reported by an earlier
    /// phase re-running the same query.
    pub fn record_errors(&mut self, module: &[Ident], errors: Vec<AnalysisError>) -> bool {
        if errors.is_empty() {
            return false;
        }
        for error in errors {
            self.error(module, error);
        }
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
    fn enclosing_analysis_site(&self) -> Option<omega_diagnostics::SourceSpan> {
        let (module, site) = self.analysis_stack.last()?;
        self.site(module, site.span)
    }

    pub(crate) fn with_analyzer<R>(
        &mut self,
        module: &[Ident],
        generics: &[(Ident, ResolvedType)],
        owner: AnalysisSite,
        f: impl FnOnce(&mut Analyzer) -> R,
    ) -> AnalyzerRun<R> {
        self.with_analyzer_in(module, generics, &[], owner, f)
    }

    pub(crate) fn with_analyzer_in<R>(
        &mut self,
        module: &[Ident],
        generics: &[(Ident, ResolvedType)],
        bounds: &[omega_analyzer::resolved_type::ResolvedBound],
        owner: AnalysisSite,
        f: impl FnOnce(&mut Analyzer) -> R,
    ) -> AnalyzerRun<R> {
        let target = self.target;
        // A substitution means this run is one concrete instantiation, not the
        // declaration itself: what is true here need not be true of the
        // written source.
        let mode = if generics.is_empty() {
            AnalysisMode::Declaration
        } else {
            AnalysisMode::GenericInstantiation {
                requested_at: self.enclosing_analysis_site(),
            }
        };
        self.analysis_stack.push((module.to_vec(), owner));
        let mut analyzer =
            Analyzer::new_in(self, module.to_vec(), generics, bounds, owner, target).in_mode(mode);
        let result = f(&mut analyzer);
        let (errors, warnings, field_usage) = analyzer.finish();
        self.analysis_stack.pop();
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
        owner: AnalysisSite,
        f: impl FnOnce(&mut Analyzer) -> R,
    ) -> R {
        let run = self.with_analyzer(module, generics, owner, f);
        self.diagnostics.record_warnings(module, run.warnings);
        run.result
    }
}
