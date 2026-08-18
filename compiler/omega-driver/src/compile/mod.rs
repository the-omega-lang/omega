use crate::conformances::ConformanceOrigin;
use crate::error::{CompileError, CompiledProgram};
use crate::items::{CheckedBody, GlueSignature, ItemKey};
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::Target;
use omega_analyzer::analysis::AnalysisSite;
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedExternDeclaration, CheckedItem, CheckedModule, ExternFunctionKind, ExternFunctionRef,
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

fn fatal(error: ResolveError) -> Vec<CompileError> {
    vec![CompileError::Resolve {
        error,
        importer: None,
    }]
}

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
        let local = self.local_module_paths().map_err(|e| vec![e])?;
        // Reject an empty package before semantic sweeps so the user gets the
        // direct package error instead of secondary resolution failures.
        if local.is_empty() {
            return Err(vec![self.empty_package_error()]);
        }
        let scanned = self.collect_extern_signatures()?;
        let compilation = CompilationModules::new(local, scanned);

        let relationship_surface = compilation.relationship_surface();
        self.collect_primitive_signatures(&relationship_surface);
        self.collect_conformance_signatures(&relationship_surface);
        self.collect_signatures(compilation.emitted())?;
        self.collect_glue_signatures(&relationship_surface);
        let (mut modules, mut warnings) = self.check_bodies(compilation.emitted())?;

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

        let diagnostic_surface = compilation.diagnostic_surface();
        let errors = self.diagnostics.drain_errors(&diagnostic_surface);
        if !errors.is_empty() {
            return Err(errors);
        }
        warnings.extend(self.diagnostics.drain_warnings(compilation.emitted()));

        let mut usage = self.diagnostics.take_comp_field_usage();
        for (_, checked_module) in &modules {
            dead_code::collect_module(checked_module, &mut usage);
        }
        warnings.extend(self.sweep_dead_code(compilation.emitted(), &usage));

        let (gap_warnings, gap_errors) = self.sweep_gaps();
        if !gap_errors.is_empty() {
            return Err(gap_errors);
        }
        warnings.extend(gap_warnings);

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

    fn local_module_paths(&mut self) -> Result<Vec<ModulePath>, CompileError> {
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

        let mut paths: Vec<ModulePath> = Vec::new();
        for (path, location) in entries {
            match location {
                Ok(location) if location.own_file.is_some() => paths.push(path),
                Ok(_) => {} // namespace-only directory -- no module of its own
                Err(error) => return Err(self.load_failure(&path, error, None)),
            }
        }
        paths.sort_by(|a, b| a.iter().map(Ident::as_ref).cmp(b.iter().map(Ident::as_ref)));

        for path in &paths {
            if let Err(error) = self.parse_module(path) {
                return Err(self.load_failure(path, error, None));
            }
        }
        Ok(paths)
    }
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
