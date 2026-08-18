//! The two-phase whole-program sweep: every reachable item's signature
//! first, then every reachable item's body.
//!
//! The split is what makes declaration order irrelevant -- same- and
//! cross-module forward references and self-references all resolve regardless
//! of which module they cross -- and mirrors the identical split
//! `omega_codegen::Codegen` does one layer down, for the same underlying
//! reason: a cross-module reference in either direction must never need
//! something that isn't ready yet.

use crate::error::{CompileError, CompiledProgram};
use crate::items::{CheckedBody, GlueSignature, ItemKey};
use crate::conformances::ConformanceOrigin;
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedExternDeclaration, CheckedItem, CheckedModule, ExternFunctionKind, ExternFunctionRef,
    Storage,
};
use omega_analyzer::dead_code::{self, FieldUsage};
use omega_analyzer::error::{
    AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind,
};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_analyzer::resolver::{ResolveError, ResolvedItem};
use omega_hir::{HirEnumDef, HirGenericParam, HirGlueDef, HirId, HirItem, HirParam, HirStructDef, HirUnionDef};
use omega_analyzer::Target;
use omega_parser::prelude::Ident;
use std::cell::RefCell;
use std::rc::Rc;

/// Every warning found in a module, tagged with it so the CLI can render it
/// against the right source file.
type TaggedWarnings = Vec<(ModulePath, AnalysisWarning)>;

/// Every compiled module, tagged with its absolute path -- what codegen
/// consumes, and what the post-phase merges append to.
pub(crate) type CheckedModules = Vec<(ModulePath, CheckedModule)>;

/// A resolution failure with no importing site of its own to blame.
fn fatal(error: ResolveError) -> Vec<CompileError> {
    vec![CompileError::Resolve {
        error,
        importer: None,
    }]
}

impl Driver {
    /// Compiles every module the local package contains (see
    /// `local_module_paths`) -- `entry` names which one has `main`, nothing
    /// more; it plays no role in *discovering* what gets compiled.
    ///
    /// A *generic template* is skipped by both phases -- it has no concrete
    /// signature or body of its own, only a specific instantiation does,
    /// triggered lazily by whatever use site first needs it. Every
    /// instantiation discovered along the way is merged into its originating
    /// module during final assembly, once both phases have fully finished (so
    /// however late one was discovered, it is guaranteed present by then).
    ///
    /// `target` re-sets the compilation target for this run (see
    /// `Driver::new`'s doc comment) -- everything this compilation does,
    /// from `comp` evaluation to codegen, answers width questions against
    /// it.
    pub fn compile(
        &mut self,
        entry: &[Ident],
        target: Target,
    ) -> Result<CompiledProgram, Vec<CompileError>> {
        self.target = target;
        let local = self.local_module_paths().map_err(|e| vec![e])?;
        // Checked before anything else runs, so an empty package reports
        // that fact directly rather than panicking later when the
        // generic-instantiation merge indexes `modules` expecting the entry
        // module to be present.
        if local.is_empty() {
            return Err(vec![self.empty_package_error()]);
        }
        let extern_surface = self.collect_extern_signatures()?;

        // Deduplicated, because sweeping a module for `glue` blocks is not
        // idempotent -- each pass appends to `items.glues`. The two lists
        // can genuinely overlap: registering a package as its own `--extern`
        // puts the identical `ModulePath` in both.
        let mut glue_modules: Vec<ModulePath> =
            Vec::with_capacity(extern_surface.len() + local.len());
        for path in extern_surface.iter().chain(local.iter()) {
            if !glue_modules.contains(path) {
                glue_modules.push(path.clone());
            }
        }
        self.collect_primitive_signatures(&glue_modules);
        self.collect_conformance_signatures(&glue_modules);
        self.collect_signatures(&local)?;
        self.collect_glue_signatures(&glue_modules);
        let (mut modules, mut warnings) = self.check_bodies(&local)?;

        // Merged only now that both phases have finished, in the order
        // instantiations were discovered. An instantiation whose template is
        // declared in an `--extern` package has no matching entry in
        // `modules` (which only holds local modules) -- falls back to the
        // first local module rather than being dropped, since this
        // compilation is the only one that will ever produce its body.
        // Safe to regroup this way because `lower_program` lowers each
        // module independently with no cross-module state.
        for (key, body) in &self.items.generic_instantiations {
            let target_index = modules
                .iter()
                .position(|(path, _)| *path == key.module)
                .unwrap_or(0);
            let (path, checked_module) = modules
                .get_mut(target_index)
                .expect("`local_module_paths` always includes at least the entry module");
            checked_module.items.push(body.item.clone());
            warnings.extend(body.warnings.iter().map(|w| (path.clone(), w.clone())));
        }
        // Every generic primitive/conform body any of the above actually
        // triggered, directly or transitively through another one's body.
        self.drain_pending_declaration_bodies(&mut modules, &mut warnings);

        // A genuine error inside an extern tree must still surface even when
        // nothing local imports that submodule. Warnings stay scoped to
        // local modules only, deliberately.
        let mut error_scope = local.clone();
        error_scope.extend(extern_surface);
        let errors = self.diagnostics.drain_errors(&error_scope);
        if !errors.is_empty() {
            return Err(errors);
        }
        warnings.extend(self.diagnostics.drain_warnings(&local));

        let mut usage = self.diagnostics.take_comp_field_usage();
        for (_, checked_module) in &modules {
            dead_code::collect_module(checked_module, &mut usage);
        }
        warnings.extend(self.sweep_dead_code(&local, &usage));

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

    /// `CompileError::EmptyPackage` for the package currently being compiled.
    fn empty_package_error(&self) -> CompileError {
        let (root, expected) = self.roots.local_root();
        CompileError::EmptyPackage { root, expected }
    }

    /// Every module the local package contains, unconditionally -- the
    /// filesystem is the source of truth, so nothing needs to *import* a
    /// sibling module for it to be part of the build, only to *reference*
    /// it. An `--extern` dependency is the opposite: resolved lazily, one
    /// path at a time, on demand.
    ///
    /// A namespace-only directory contributes no module of its own. Each
    /// real module is parsed here, not merely inventoried, so a genuine
    /// parse/macro-expansion failure anywhere in the package is caught with
    /// full diagnostic detail rather than falling through to a generic
    /// `ResolveError` the first time something references it.
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
        // Deterministic order: `local_modules()`'s backing map guarantees
        // none, and `collect_signatures` mints globally-sequential synthetic
        // ids as it visits modules -- a random order would bake a different
        // id onto each instantiation build-to-build.
        paths.sort_by(|a, b| a.iter().map(Ident::as_ref).cmp(b.iter().map(Ident::as_ref)));

        for path in &paths {
            if let Err(error) = self.parse_module(path) {
                return Err(self.load_failure(path, error, None));
            }
        }
        Ok(paths)
    }

    /// Every registered `--extern`'s own struct/spec surface -- signatures
    /// only, never a body -- eagerly resolved regardless of whether
    /// anything actually imports or path-references them, so `sweep_gaps`
    /// sees the whole picture instead of missing an unimported `glue`.
    /// Returns every module path visited, for `compile`'s `error_scope`.
    /// Restricted to `HirItem::Struct`/`HirItem::Spec`/`HirItem::Gap`, the
    /// only signatures gap resolution needs.
    ///
    /// A parse or signature failure here is a real, fatal `CompileError`,
    /// not swallowed -- so a broken, wholly unrelated struct/spec anywhere
    /// in any registered extern can fail a build that never references it.
    /// See docs/architecture/module-driver-and-linkage.md for the full rationale and cost.
    fn collect_extern_signatures(&mut self) -> Result<Vec<ModulePath>, Vec<CompileError>> {
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
                let _ = self.ensure_item(path, path, &name, &[], true, false);
            }
        }
        Ok(paths)
    }

    fn collect_glue_signatures(&mut self, paths: &[ModulePath]) {
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
                let run = self.with_analyzer(path, &[], (glue.id, glue.span), |analyzer| {
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
                        if let Some(previous) = seen.insert(function.name.clone(), function.span) {
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
                                // The signature, not the whole `glue`
                                // block: a glue member is never wrapped in
                                // an `ItemNode`, so `function.span` is the
                                // enclosing block's.
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
                            // The name -- an unexpected member is an
                            // identity problem, not a whole-block one.
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
                });
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

    fn same_glue_signature(expected: &ResolvedFunctionType, actual: &ResolvedFunctionType) -> bool {
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

    /// Every `gap` this compilation actually resolved (local or
    /// `--extern`), checked against every `glue` resolved alongside it:
    /// zero matches is `UnfilledGap` (a warning), two or more is
    /// `MultipleGluesForGap` (an error). Returns `CompileError`s directly,
    /// bypassing `Diagnostics`' per-module scope filtering -- a gap/glue
    /// conflict is a whole-program fact belonging to neither side's module.
    fn sweep_gaps(&self) -> (TaggedWarnings, Vec<CompileError>) {
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

    /// Phase 1: every local module's every non-generic item's signature.
    ///
    /// A same- or cross-module by-value cycle is rejected right at the item
    /// that closes it, without affecting any other item.
    fn collect_signatures(&mut self, local: &[ModulePath]) -> Result<(), Vec<CompileError>> {
        for path in local {
            self.ensure_module_indexed(path).map_err(fatal)?;

            // Bound-position spec references only resolve at instantiation
            // time, which this package's own build may never do for a given
            // template -- mark their import aliases as used at declaration
            // time so `UnusedImport` doesn't report the import that binds a
            // bound's name.
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

            // Items are visited in declaration order because this sweep
            // mints globally-sequential synthetic ids as a side effect -- a
            // random order would bake a different id onto each instantiation
            // build-to-build.
            for (name, _) in self.modules.index(path).plain_items() {
                if self.is_generic_template(path, &name).map_err(fatal)? {
                    continue;
                }
                // Nothing is in progress at this point in the sweep, so
                // `indirect`'s distinction cannot matter here.
                let _ = self.ensure_item(path, path, &name, &[], true, false);
            }

            // Unlike a generic instantiation, an overload set is fully
            // enumerable up front, so every candidate's signature is resolved
            // eagerly here rather than on demand.
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

    /// Phase 2: every local module's every non-generic item's body, now that
    /// every local signature is guaranteed to exist. `local` never contains
    /// an `--extern` path (see `local_module_paths`) -- an extern module's
    /// *ordinary* items are never body-checked or defined by this
    /// compilation at all; only a generic instantiation of one of its
    /// templates is (merged during final assembly), since nothing else will
    /// ever compile that exact instantiation, and that must happen in
    /// whichever project actually asked for it.
    fn check_bodies(
        &mut self,
        local: &[ModulePath],
    ) -> Result<(CheckedModules, TaggedWarnings), Vec<CompileError>> {
        let mut modules = Vec::with_capacity(local.len());
        let mut warnings = TaggedWarnings::new();

        for path in local {
            let items = self.check_module_bodies(path, &mut warnings)?;
            let id = self.modules.parsed(path).id;
            modules.push((path.clone(), CheckedModule { id, items }));
        }

        Ok((modules, warnings))
    }

    /// One local module's checked items, in declaration order -- which is the
    /// order codegen then declares and defines them in.
    fn check_module_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Result<Vec<CheckedItem>, Vec<CompileError>> {
        let mut bodies: Vec<CheckedBody> = Vec::new();

        for (name, index) in self.modules.index(path).plain_items() {
            if self.is_generic_template(path, &name).map_err(fatal)? {
                continue;
            }
            bodies.extend(self.ensure_item_body(&ItemKey::new(path, &name, &[]), index));
        }

        for indices in self.modules.index(path).overloads.clone().into_values() {
            for index in indices {
                bodies.extend(self.ensure_overload_body(path, index));
            }
        }

        let mut items = Vec::with_capacity(bodies.len());
        for body in bodies {
            items.push(body.item);
            warnings.extend(body.warnings.into_iter().map(|w| (path.to_vec(), w)));
        }
        items.extend(self.synthesize_gap_items(path));
        items.extend(self.check_glue_bodies(path, warnings));
        items.extend(self.check_primitive_bodies(path, warnings));
        items.extend(self.check_conformance_bodies(path, warnings));
        self.report_unused_imports(path, warnings);
        Ok(items)
    }

    fn check_glue_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let signatures: Vec<(
            std::rc::Rc<omega_analyzer::resolved_type::ResolvedGap>,
            Vec<(omega_hir::HirFunctionDef, ResolvedFunctionType)>,
        )> = self
            .items
            .glues
            .iter()
            .filter(|glue| glue.module == path)
            .map(|glue| (glue.gap.clone(), glue.functions.clone()))
            .collect();
        let mut items = Vec::new();
        for (gap, functions) in signatures {
            for (function, fn_type) in functions {
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
                    mangling: ManglingMode::Glued {
                        spec_module_path: gap.module_path.clone(),
                        spec_name: gap.name.clone(),
                        function_name: function.name.clone(),
                    },
                    ..Default::default()
                };
                let run = self.with_analyzer(path, &[], (function.id, function.span), |analyzer| {
                    analyzer.check_function_body(&function, &fn_type, function.id, &annotations)
                });
                if let Some(checked) = run.result {
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
        }
        items
    }

    fn check_conformance_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let entries: Vec<_> = self
            .conformances
            .entries
            .iter()
            .filter(|entry| {
                entry.module == path
                    // A concrete conformance owned by an extern package is
                    // defined by that package's object, never by whichever
                    // consumer needed it. Generic conformances are the
                    // exception: their instantiation is monomorphized here.
                    && (!self.roots.is_extern(&entry.module) || entry.origin != ConformanceOrigin::Concrete)
                    && !self.conformances.emitted.contains(&(
                        entry.target.clone(),
                        entry.spec.borrow().id,
                        entry.spec_args.clone(),
                    ))
            })
            .cloned()
            .collect();
        let mut items = Vec::new();
        for entry in entries {
            self.conformances.emitted.push((
                entry.target.clone(),
                entry.spec.borrow().id,
                entry.spec_args.clone(),
            ));
            let mut bounds = vec![(
                entry.target.clone(),
                entry.spec.clone(),
                entry.spec_args.clone(),
            )];
            let keys_run = self.with_analyzer(
                &entry.module,
                &entry.substitution,
                (entry.id, entry.span),
                |a| a.expand_bound_set(entry.id, entry.span, &entry.declared_bounds),
            );
            self.diagnostics.record_warnings(&entry.module, keys_run.warnings);
            let keys = keys_run.result;
            bounds.extend(self.bound_context_over(&entry.declared_bounds, &keys));
            let owner = Self::conformance_owner(&entry);
            for (function, method_id) in entry.functions.iter().zip(&entry.method_ids) {
                let Some((_, method)) = entry
                    .methods
                    .iter()
                    .find(|(_, method)| method.decl_id == *method_id)
                else {
                    continue;
                };
                let run = self.with_analyzer_in(
                    path,
                    &entry.substitution,
                    &bounds,
                    (function.id, function.span),
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
                    checked.conformance_owner = Some(owner.clone());
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
            for pending in &entry.pending {
                let run = self.with_analyzer_in(
                    path,
                    &pending.substitution,
                    &bounds,
                    (pending.id, pending.raw.span),
                    |analyzer| analyzer.check_pending_spec_method(pending),
                );
                if let Some(mut checked) = run.result {
                    checked.conformance_owner = Some(owner.clone());
                    items.push(CheckedItem::FunctionDefinition(checked));
                }
                warnings.extend(
                    run.warnings
                        .into_iter()
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
        }
        items
    }

    fn check_primitive_bodies(
        &mut self,
        path: &[Ident],
        warnings: &mut TaggedWarnings,
    ) -> Vec<CheckedItem> {
        let entries: Vec<_> = self
            .primitives
            .entries
            .iter()
            .filter(|entry| {
                entry.module == path && !self.primitives.emitted.contains(&entry.target)
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
                    (function.id, function.span),
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
                        .map(|warning| (path.to_vec(), warning)),
                );
            }
        }
        items
    }

    fn drain_pending_declaration_bodies(
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
                        && (!self.roots.is_extern(&entry.module)
                            || entry.monomorphized())
                })
                .map(|entry| entry.module.clone());
            let conformance_module = self
                .conformances
                .entries
                .iter()
                .find(|entry| {
                    !self.conformances.emitted.contains(&(
                            entry.target.clone(),
                            entry.spec.borrow().id,
                            entry.spec_args.clone(),
                        ))
                        && (!self.roots.is_extern(&entry.module) || entry.origin != ConformanceOrigin::Concrete)
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
            if let Some((_, checked)) = modules.iter_mut().find(|(path, _)| *path == module) {
                checked.items.extend(items);
            } else if let Some((_, checked)) = modules.first_mut() {
                checked.items.extend(items);
            }
        }
    }

    /// Every `gap` declared directly in `path`, synthesized as one
    /// `CheckedItem::ExternDeclaration` per required function -- at codegen
    /// time a gap's required function is indistinguishable from a
    /// hand-written top-level `extern`, just with its symbol forced to
    /// match its `glue` implementation (`ManglingMode::Glued`). This is the
    /// one place a gap's own functions turn into anything codegen can see.
    fn synthesize_gap_items(&mut self, path: &[Ident]) -> Vec<CheckedItem> {
        let mut items = Vec::new();
        for (name, index) in self.modules.index(path).plain_items() {
            if matches!(self.modules.parsed(path).hir.items[index], HirItem::Gap(_)) {
                let key = ItemKey::new(path, &name, &[]);
                let Some(gap) = self.items.gaps.get(&key) else {
                    continue;
                };
                for (fn_name, gap_fn) in &gap.functions {
                    items.push(CheckedItem::ExternDeclaration(CheckedExternDeclaration {
                        id: gap_fn.decl_id,
                        span: gap_fn.span,
                        ident: fn_name.clone(),
                        r#type: ResolvedType::Function(gap_fn.fn_type.clone()),
                        mangling: ManglingMode::Glued {
                            spec_module_path: gap.module_path.clone(),
                            spec_name: gap.name.clone(),
                            function_name: fn_name.clone(),
                        },
                    }));
                }
                continue;
            }
        }
        items
    }

    /// Every alias this module declared that no reference ever looked up, now
    /// that its whole body is checked -- an alias used only inside a method
    /// body is exactly why this cannot run any earlier.
    fn report_unused_imports(&mut self, path: &[Ident], warnings: &mut TaggedWarnings) {
        for (alias, import) in &self.modules.index(path).imports {
            if self.imports.was_used(path, alias) {
                continue;
            }
            let kind = AnalysisWarningKind::UnusedImport {
                alias: alias.clone(),
            };
            if import.suppress.iter().any(|s| s.as_ref() == kind.name()) {
                continue;
            }
            warnings.push((
                path.to_vec(),
                AnalysisWarning::new(import.id, import.span, kind),
            ));
        }
    }

    /// Every extern-owned, *non-generic* function/method this compilation
    /// referenced -- everything codegen must declare (never define) a link
    /// against. Swept once, at the end, directly over the already-populated
    /// per-item caches: anything actually referenced is sitting in them by
    /// construction, so nothing dedicated is tracked in the hot path.
    ///
    /// A *generic* instantiation of an extern template is deliberately
    /// excluded: it's fully compiled locally instead, since no other
    /// compilation will ever produce it.
    fn collect_extern_functions(&self) -> Vec<ExternFunctionRef> {
        let mut functions = Vec::new();

        for (key, item) in self.items.resolved_items() {
            if key.is_instantiation() || !self.roots.is_extern(&key.module) {
                continue;
            }
            let ResolvedItem::Value {
                r#type: ResolvedType::Function(fn_type),
                storage: Storage::Function,
                decl_id,
                mutable: _,
            } = item
            else {
                continue;
            };
            functions.push(ExternFunctionRef {
                decl_id: *decl_id,
                module_path: key.module.clone(),
                kind: ExternFunctionKind::Free(key.name.clone()),
                fn_type: fn_type.clone(),
                mangling: self.mangling_of(decl_id),
            });
        }

        // Free-function *overloads* live in their own cache, addressed by
        // position rather than by name -- the function's own name/id are read
        // back off the parsed HIR at that same position.
        for ((module_path, index), fn_type) in &self.items.overload_signatures {
            if !self.roots.is_extern(module_path) {
                continue;
            }
            let HirItem::FunctionDefinition(f) =
                &self.modules.parsed(module_path).hir.items[*index]
            else {
                unreachable!("only a function is ever recorded as an overload candidate");
            };
            functions.push(ExternFunctionRef {
                decl_id: f.id,
                module_path: module_path.clone(),
                kind: ExternFunctionKind::Free(f.name.clone()),
                fn_type: fn_type.clone(),
                mangling: self.mangling_of(&f.id),
            });
        }

        for (key, methods) in self.items.cells.all_methods() {
            if key.is_instantiation() || !self.roots.is_extern(&key.module) {
                continue;
            }
            for (method_name, method) in methods {
                functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: key.module.clone(),
                    kind: ExternFunctionKind::Method {
                        type_name: key.name.clone(),
                        method_name,
                    },
                    mangling: method.annotations.mangling,
                    fn_type: method.fn_type,
                });
            }
        }

        for (key, gap) in &self.items.gaps {
            if !self.roots.is_extern(&key.module) {
                continue;
            }
            for (fn_name, gap_fn) in &gap.functions {
                functions.push(ExternFunctionRef {
                    decl_id: gap_fn.decl_id,
                    module_path: gap.module_path.clone(),
                    kind: ExternFunctionKind::Free(fn_name.clone()),
                    fn_type: gap_fn.fn_type.clone(),
                    mangling: ManglingMode::Glued {
                        spec_module_path: gap.module_path.clone(),
                        spec_name: gap.name.clone(),
                        function_name: fn_name.clone(),
                    },
                });
            }
        }

        for entry in &self.primitives.entries {
            if entry.monomorphized() || !self.roots.is_extern(&entry.module) {
                continue;
            }
            for (method_name, method) in &entry.methods {
                functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: entry.module.clone(),
                    kind: ExternFunctionKind::Primitive {
                        target: entry.target.clone(),
                        method_name: method_name.clone(),
                    },
                    fn_type: method.fn_type.clone(),
                    mangling: method.annotations.mangling.clone(),
                });
            }
        }

        for entry in &self.conformances.entries {
            if entry.origin != ConformanceOrigin::Concrete || !self.roots.is_extern(&entry.module) {
                continue;
            }
            for (method_name, method) in &entry.methods {
                if method.source.is_none() {
                    continue;
                }
                functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: entry.module.clone(),
                    kind: ExternFunctionKind::Conform {
                        target: entry.target.clone(),
                        spec_name: entry.spec.borrow().name.clone(),
                        spec_args: entry.spec_args.clone(),
                        method_name: method_name.clone(),
                    },
                    fn_type: method.fn_type.clone(),
                    mangling: method.annotations.mangling.clone(),
                });
            }
        }

        functions
    }

    /// A free function's own resolved `@mangling(...)`, which a consuming
    /// compilation must agree with or the two mangled symbols diverge.
    fn mangling_of(&self, decl_id: &HirId) -> ManglingMode {
        self.items
            .function_annotations
            .get(decl_id)
            .map(|a| a.mangling.clone())
            .unwrap_or_default()
    }

    /// `UnusedField`/`NeverConstructedVariant`'s whole-program sweep, run once
    /// every reachable module's items are checked and diffed against every
    /// *local* type's own declared fields/variants.
    ///
    /// A generic template's instantiations are one *declaration* between
    /// them, so they're judged together: a field is only unused if no
    /// instantiation ever touched it, and it is reported once, not once per
    /// instantiation.
    ///
    /// Scoped to local modules: an extern-owned type's "unused" status only
    /// reflects what this compilation happens to touch, not what a
    /// downstream consumer might, so warning on it would be a false positive.
    ///
    /// Enum *header* fields are deliberately never checked at all (there is
    /// no `usage.enum_header_fields` to check against) -- they are per-variant
    /// compile-time constants, not storage, so "never read" is a far weaker
    /// signal for them than for an ordinary field.
    fn sweep_dead_code(&self, local: &[ModulePath], usage: &FieldUsage) -> TaggedWarnings {
        let mut warnings = TaggedWarnings::new();

        let unused_field = |owner: &Ident, field: &HirParam| {
            AnalysisWarning::new(
                field.id,
                field.span,
                AnalysisWarningKind::UnusedField {
                    owner: owner.clone(),
                    field: field.ident.clone(),
                },
            )
        };

        for decl in group_by_declaration(self.items.cells.structs(), |c| (c.id, c.suppress.clone()))
        {
            let Some(def) = self.hir_struct(decl.module, decl.name) else {
                continue;
            };
            if !local.contains(decl.module) || decl.suppresses("unused_field") {
                continue;
            }
            for (index, field) in def.fields.iter().enumerate() {
                if !decl.any(|id| usage.struct_fields.contains(&(id, index))) {
                    warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                }
            }
        }

        for decl in group_by_declaration(self.items.cells.unions(), |c| (c.id, c.suppress.clone()))
        {
            let Some(def) = self.hir_union(decl.module, decl.name) else {
                continue;
            };
            if !local.contains(decl.module) || decl.suppresses("unused_field") {
                continue;
            }
            for (index, field) in def.fields.iter().enumerate() {
                if !decl.any(|id| usage.union_fields.contains(&(id, index))) {
                    warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                }
            }
        }

        for decl in group_by_declaration(self.items.cells.enums(), |c| (c.id, c.suppress.clone())) {
            let Some(def) = self.hir_enum(decl.module, decl.name) else {
                continue;
            };
            if !local.contains(decl.module) {
                continue;
            }

            if !decl.suppresses("unused_field") {
                for (index, field) in def.dynamic_fields.iter().enumerate() {
                    if !decl.any(|id| usage.enum_dynamic_fields.contains(&(id, index))) {
                        warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                    }
                }
                for (variant_index, variant) in def.variants.iter().enumerate() {
                    for (field_index, field) in variant.fields.iter().enumerate() {
                        if !decl.any(|id| {
                            usage
                                .enum_body_fields
                                .contains(&(id, variant_index, field_index))
                        }) {
                            warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                        }
                    }
                }
            }

            if !decl.suppresses("never_constructed_variant") {
                for (variant_index, variant) in def.variants.iter().enumerate() {
                    if decl.any(|id| usage.enum_variants.contains(&(id, variant_index))) {
                        continue;
                    }
                    warnings.push((
                        decl.module.clone(),
                        AnalysisWarning::new(
                            variant.id,
                            variant.span,
                            AnalysisWarningKind::NeverConstructedVariant {
                                r#enum: decl.name.clone(),
                                variant: variant.name.clone(),
                            },
                        ),
                    ));
                }
            }
        }

        // Each of the three loops above is already deterministic on its own
        // (the cell caches preserve creation order). This sort is for
        // something they can't give separately: one chronological ordering
        // across all three kinds together, instead of every struct warning,
        // then every union warning, then every enum warning.
        warnings.sort_by(|(a_path, a), (b_path, b)| {
            let key = |path: &ModulePath| {
                path.iter()
                    .map(|i| i.as_ref().to_string())
                    .collect::<Vec<_>>()
            };
            key(a_path)
                .cmp(&key(b_path))
                .then(a.span.start.cmp(&b.span.start))
        });
        warnings
    }

    /// The raw HIR a type cell's declaration came from, for the real
    /// per-field/per-variant *spans* dead-code reporting needs: the resolved
    /// field lists drop spans the moment a field is resolved (nothing
    /// downstream has ever needed them back), while the HIR nodes keep them.
    fn hir_struct(&self, module: &[Ident], name: &Ident) -> Option<&HirStructDef> {
        match self.modules.item(module, name)? {
            HirItem::Struct(s) => Some(s),
            _ => None,
        }
    }

    fn hir_union(&self, module: &[Ident], name: &Ident) -> Option<&HirUnionDef> {
        match self.modules.item(module, name)? {
            HirItem::Union(u) => Some(u),
            _ => None,
        }
    }

    fn hir_enum(&self, module: &[Ident], name: &Ident) -> Option<&HirEnumDef> {
        match self.modules.item(module, name)? {
            HirItem::Enum(e) => Some(e),
            _ => None,
        }
    }
}

/// One type *declaration* as dead-code analysis sees it: every cell that
/// instantiated it (exactly one for an ordinary, non-generic type), reduced
/// to the two facts the sweep needs.
struct Declaration<'a> {
    module: &'a ModulePath,
    name: &'a Ident,
    ids: Vec<HirId>,
    /// The declaration's own `@suppress(...)` list -- identical across every
    /// instantiation, since it's a property of the declaration.
    suppress: Vec<Ident>,
}

impl Declaration<'_> {
    fn suppresses(&self, warning: &str) -> bool {
        self.suppress.iter().any(|s| s.as_ref() == warning)
    }

    /// Whether *any* instantiation of this declaration satisfies `used` --
    /// a field touched through one instantiation is not dead code just
    /// because another instantiation never touched it.
    fn any(&self, used: impl Fn(HirId) -> bool) -> bool {
        self.ids.iter().copied().any(used)
    }
}

/// Groups type cells by the declaration they came from, in first-creation
/// order. `facts` pulls the per-cell identity and suppress list out of
/// whichever of the three cell types this is.
fn group_by_declaration<'a, T>(
    cells: impl Iterator<Item = (&'a ItemKey, &'a Rc<RefCell<T>>)>,
    facts: impl Fn(&T) -> (HirId, Vec<Ident>),
) -> Vec<Declaration<'a>>
where
    T: 'a,
{
    let mut grouped: IndexMap<(&ModulePath, &Ident), Declaration<'a>> = IndexMap::new();
    for (key, cell) in cells {
        let (id, suppress) = facts(&cell.borrow());
        grouped
            .entry((&key.module, &key.name))
            .or_insert_with(|| Declaration {
                module: &key.module,
                name: &key.name,
                ids: vec![],
                suppress,
            })
            .ids
            .push(id);
    }
    grouped.into_values().collect()
}
