use crate::conformances::ConformanceOrigin;
use crate::error::{CompileError, CompiledProgram};
use crate::items::{CheckedBody, GlueSignature, ItemKey};
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::Target;
use omega_analyzer::analysis::AnalysisSite;
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedForeignBinding, CheckedItem, CheckedModule, ExternFunctionKind, ExternFunctionRef,
    Storage,
};
use omega_analyzer::dead_code::{self, FieldUsage};
use omega_analyzer::error::{
    AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind,
};
use omega_analyzer::resolved_type::{ResolvedBound, ResolvedFunctionType, ResolvedType};
use omega_analyzer::resolver::{ResolveError, ResolveItemOptions, ResolvedItem};
use omega_hir::{
    HirEnumDef, HirField, HirGenericParam, HirGlueDef, HirId, HirItem, HirStructDef, HirUnionDef,
};
use omega_parser::prelude::Ident;
use std::cell::RefCell;
use std::rc::Rc;

pub(super) type TaggedWarnings = Vec<(ModulePath, AnalysisWarning)>;

pub(super) type CheckedModules = Vec<(ModulePath, CheckedModule)>;

struct CompilationModules {
    emitted: Vec<ModulePath>,
    scanned: Vec<ModulePath>,
}

impl CompilationModules {
    fn new(emitted: Vec<ModulePath>, scanned: Vec<ModulePath>) -> Self {
        Self { emitted, scanned }
    }

    fn emitted(&self) -> &[ModulePath] {
        &self.emitted
    }

    fn relationship_surface(&self) -> Vec<ModulePath> {
        Self::dedup(self.scanned.iter().chain(&self.emitted))
    }

    fn diagnostic_surface(&self) -> Vec<ModulePath> {
        Self::dedup(self.emitted.iter().chain(&self.scanned))
    }

    fn dedup<'a>(paths: impl IntoIterator<Item = &'a ModulePath>) -> Vec<ModulePath> {
        let mut result = Vec::new();
        for path in paths {
            if !result.contains(path) {
                result.push(path.clone());
            }
        }
        result
    }
}

mod bodies;
mod output;
mod signatures;

impl Driver {
    pub fn compile(
        &mut self,
        entry: &[Ident],
        target: Target,
    ) -> Result<CompiledProgram, Vec<CompileError>> {
        self.target = target;
        let Some(local) = self.local_module_paths() else {
            // Reject an empty package before semantic sweeps so the user gets
            // the direct package error instead of secondary resolution
            // failures.
            return Err(vec![self.empty_package_error()]);
        };
        let scanned = self.collect_extern_signatures();
        let compilation = CompilationModules::new(local, scanned);

        let relationship_surface = compilation.relationship_surface();
        self.collect_primitive_signatures(&relationship_surface);
        self.collect_conformance_signatures(&relationship_surface);
        self.collect_signatures(compilation.emitted(), entry);
        self.collect_glue_signatures(&relationship_surface);
        let (mut modules, mut warnings) = self.check_bodies(compilation.emitted());

        // Concrete generic bodies are emitted by this invocation even when
        // their template lives in an extern package. Keep the template's
        // declared module path: MIR uses that path as part of symbol identity.
        for (key, body) in &self.items.generic_instantiations {
            let checked_module = self.emission_module(&mut modules, &key.module);
            checked_module.items.push(body.item.clone());
            warnings.extend(
                body.warnings
                    .iter()
                    .map(|warning| (key.module.clone(), warning.clone())),
            );
        }
        self.drain_pending_declaration_bodies(&mut modules, &mut warnings);

        // The last relationship sweep that can still produce errors, so it
        // runs before the barrier; its warnings are absence claims and wait
        // until the frontend is known to be clean.
        let (gap_warnings, gap_errors) = self.sweep_gaps();
        for error in gap_errors {
            self.diagnostics.fail(None, error);
        }

        debug_assert!(
            self.items.failures_retain_a_cause(),
            "a failed item query must retain why it failed, so no dependent lookup \
             can turn an unreported failure into an 'already failed' message"
        );

        let diagnostic_surface = compilation.diagnostic_surface();
        let errors = self.diagnostics.drain(&diagnostic_surface);
        if !errors.is_empty() {
            return Err(errors);
        }
        warnings.extend(self.diagnostics.drain_warnings(compilation.emitted()));

        // Whole-program absence warnings only: a skipped module would make
        // every one of them a false claim, so they wait for a clean frontend.
        for path in compilation.emitted() {
            self.report_unused_imports(path, &mut warnings);
        }
        let mut usage = self.diagnostics.take_comp_field_usage();
        for (_, checked_module) in &modules {
            dead_code::collect_module(checked_module, &mut usage);
        }
        warnings.extend(self.sweep_dead_code(compilation.emitted(), &usage));
        warnings.extend(gap_warnings);
        deduplicate_warnings(&mut warnings);

        let extern_functions = self.collect_extern_functions();
        Ok(CompiledProgram {
            modules,
            entry: entry.to_vec(),
            warnings,
            extern_functions,
        })
    }

    fn empty_package_error(&self) -> CompileError {
        let (root, expected) = self.roots.local_root();
        CompileError::EmptyPackage { root, expected }
    }

    /// Every local module that parsed, with each independent discovery or
    /// parse failure recorded. `None` means the package declares no module at
    /// all, which is a package-level blocker rather than a module failure.
    fn local_module_paths(&mut self) -> Option<Vec<ModulePath>> {
        // Collected into an owned `Vec` first, not iterated in place --
        // `load_failure` below needs `&mut self`, which can't coexist with
        // `local_modules()`'s own borrow of `self.roots`.
        let entries: Vec<(
            ModulePath,
            Result<crate::fs_resolve::ModuleLocation, ResolveError>,
        )> = self
            .roots
            .local_modules()
            .map(|(path, result)| (path.clone(), result.clone()))
            .collect();

        let mut declared: Vec<ModulePath> = Vec::new();
        let mut failed: Vec<(ModulePath, ResolveError)> = Vec::new();
        for (path, location) in entries {
            match location {
                Ok(location) if location.own_file.is_some() => declared.push(path),
                Ok(_) => {} // namespace-only directory -- no module of its own
                Err(error) => failed.push((path, error)),
            }
        }
        if declared.is_empty() && failed.is_empty() {
            return None;
        }
        declared.sort_by(|a, b| a.iter().map(Ident::as_ref).cmp(b.iter().map(Ident::as_ref)));
        failed
            .sort_by(|(a, _), (b, _)| a.iter().map(Ident::as_ref).cmp(b.iter().map(Ident::as_ref)));

        for (path, error) in failed {
            let failure = self.load_failure(&path, error, None);
            self.diagnostics.fail(Some(&path), failure);
            self.diagnostics.poison(&path);
        }

        let mut parsed = Vec::with_capacity(declared.len());
        for path in declared {
            match self.parse_module(&path) {
                Ok(_) => parsed.push(path),
                Err(error) => {
                    let failure = self.load_failure(&path, error, None);
                    self.diagnostics.fail(Some(&path), failure);
                    self.diagnostics.poison(&path);
                }
            }
        }
        Some(parsed)
    }
}

/// Collapses findings that a reader could not tell apart. A generic template
/// analyzed once per instantiation, or a macro body expanded at several call
/// sites, produces the same claim about the same source construct each time;
/// a differing concrete payload keeps them distinct because that payload is
/// the useful fact.
fn deduplicate_warnings(warnings: &mut TaggedWarnings) {
    let mut seen = std::collections::HashSet::new();
    warnings.retain(|(module, warning)| {
        // Macro-authored findings are all actionable at the one definition
        // they came from, so the definition is their identity; everything else
        // is identified by the node it is about.
        let site = match &warning.authored {
            Some(authored) => (Some(authored.at), None),
            None => (None, Some((warning.node_id, warning.span))),
        };
        seen.insert((module.clone(), site, warning.kind.to_string()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> ModulePath {
        vec![Ident(name.to_string())]
    }

    #[test]
    fn compilation_surfaces_preserve_relationship_and_diagnostic_order() {
        let local = path("local");
        let shared = path("shared");
        let external = path("external");
        let modules = CompilationModules::new(
            vec![local.clone(), shared.clone()],
            vec![external.clone(), shared.clone()],
        );

        assert_eq!(
            modules.relationship_surface(),
            vec![external.clone(), shared.clone(), local.clone()]
        );
        assert_eq!(modules.diagnostic_surface(), vec![local, shared, external]);
    }
}
