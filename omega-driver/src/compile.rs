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
use crate::items::{CheckedBody, ItemKey};
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{CheckedItem, CheckedModule, ExternFunctionKind, ExternFunctionRef, Storage};
use omega_analyzer::dead_code::{self, FieldUsage};
use omega_analyzer::error::{AnalysisWarning, AnalysisWarningKind};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_analyzer::resolver::{ResolveError, ResolvedItem};
use omega_hir::{HirEnumDef, HirId, HirItem, HirParam, HirStructDef, HirUnionDef};
use omega_parser::prelude::Ident;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Every warning found in a module, tagged with it so the CLI can render it
/// against the right source file.
type TaggedWarnings = Vec<(ModulePath, AnalysisWarning)>;

/// Every compiled module, tagged with its absolute path -- what codegen
/// consumes, and what the post-phase merges append to.
pub(crate) type CheckedModules = Vec<(ModulePath, CheckedModule)>;

/// A resolution failure with no importing site of its own to blame.
fn fatal(error: ResolveError) -> Vec<CompileError> {
    vec![CompileError::Resolve { error, importer: None }]
}

impl Driver {
    /// Compiles every module reachable from `entry`.
    ///
    /// A *generic template* is skipped by both phases -- it has no concrete
    /// signature or body of its own, only a specific instantiation does,
    /// triggered lazily by whatever use site first needs it. Every
    /// instantiation discovered along the way is merged into its originating
    /// module during final assembly, once both phases have fully finished (so
    /// however late one was discovered, it is guaranteed present by then).
    ///
    /// `entry_file` is the concrete on-disk file backing `entry`, needed by
    /// the very first step both to register the entry's own identity for
    /// collision detection against every `--extern` and to seed the
    /// declared-name -> real-file translation every lookup goes through.
    pub fn compile(&mut self, entry: &[Ident], entry_file: &Path) -> Result<CompiledProgram, Vec<CompileError>> {
        self.roots.resolve_identities(entry, entry_file)?;
        let reachable = self.discover_reachable(entry).map_err(|e| vec![e])?;
        // An extern module's items resolve lazily, on demand, exactly like a
        // generic instantiation: "scanned, not compiled" means its signatures
        // are fully available to whatever local code references them, but
        // nothing in it is ever eagerly resolved, body-checked, or handed to
        // codegen for a *definition* -- that's the separate `omgc` invocation
        // compiling it standalone's job. Errors purely internal to an extern
        // module (a broken import nothing local ever needed, say) are
        // correspondingly never surfaced by this compilation either.
        let local: Vec<ModulePath> = reachable.iter().filter(|p| !self.roots.is_extern(p)).cloned().collect();

        self.collect_signatures(&local)?;
        let (mut modules, mut warnings) = self.check_bodies(&reachable)?;

        // Merged only now that both phases have finished, in the
        // (deterministic) order instantiations were discovered.
        for (key, body) in &self.items.generic_instantiations {
            let Some((path, checked_module)) = modules.iter_mut().find(|(path, _)| *path == key.module) else {
                continue;
            };
            checked_module.items.push(body.item.clone());
            warnings.extend(body.warnings.iter().map(|w| (path.clone(), w.clone())));
        }
        // Every `for`-attached method any of the above actually triggered,
        // directly or transitively through another one's body.
        self.drain_pending_extensions(&mut modules, &mut warnings);

        // A genuine error inside `core`'s own tree (a `for` block with a
        // bodyless function, say) must still surface even when nothing local
        // imports that particular submodule. Warnings stay scoped to local
        // modules only, deliberately.
        let mut error_scope = local.clone();
        error_scope.extend(self.extensions.module_paths.iter().cloned());
        let errors = self.diagnostics.drain_errors(&error_scope);
        if !errors.is_empty() {
            return Err(errors);
        }
        warnings.extend(self.diagnostics.drain_warnings(&local));

        let mut usage = FieldUsage::default();
        for (_, checked_module) in &modules {
            dead_code::collect_module(checked_module, &mut usage);
        }
        warnings.extend(self.sweep_dead_code(&local, &usage));

        let extern_functions = self.collect_extern_functions();
        Ok(CompiledProgram { modules, entry: entry.to_vec(), warnings, extern_functions })
    }

    /// Phase 1: every local module's every non-generic item's signature.
    ///
    /// A same- or cross-module by-value cycle is rejected right at the item
    /// that closes it, without affecting any other item.
    fn collect_signatures(&mut self, local: &[ModulePath]) -> Result<(), Vec<CompileError>> {
        for path in local {
            self.ensure_module_indexed(path).map_err(fatal)?;

            // Items are visited in declaration order (the index preserves it)
            // because this sweep mints globally-sequential synthetic ids as a
            // side effect: a random visit order would bake a different id
            // onto each instantiation build-to-build -- harmless for
            // correctness, but it's what used to make the emitted object file
            // differ byte-for-byte across repeated builds of identical source.
            for (name, _) in self.modules.index(path).plain_items() {
                if self.is_generic_template(path, &name).map_err(fatal)? {
                    continue;
                }
                // Nothing is in progress at this point in the sweep, so
                // `indirect`'s distinction cannot matter here; `true` just
                // means "no spurious cycle risk from the sweep itself".
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
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// Phase 2: every local module's every non-generic item's body, now that
    /// every reachable signature is guaranteed to exist.
    fn check_bodies(
        &mut self,
        reachable: &[ModulePath],
    ) -> Result<(CheckedModules, TaggedWarnings), Vec<CompileError>> {
        let mut modules = Vec::with_capacity(reachable.len());
        let mut warnings = TaggedWarnings::new();

        for path in reachable {
            // An extern module's *ordinary* items are never body-checked or
            // defined here -- only a generic instantiation of one of its
            // templates is (merged during final assembly): nothing else will
            // ever compile that exact instantiation, so it must happen in
            // whichever project actually asked for it.
            let items = if self.roots.is_extern(path) {
                Vec::new()
            } else {
                self.check_module_bodies(path, &mut warnings)?
            };
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
        self.reject_local_extensions(path);

        let hir = self.modules.hir(path);
        let mut bodies: Vec<CheckedBody> = Vec::new();

        for (name, index) in self.modules.index(path).plain_items() {
            if self.is_generic_template(path, &name).map_err(fatal)? {
                continue;
            }
            bodies.extend(self.check_item_body(&ItemKey::new(path, &name, &[]), &hir.items[index]));
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
        self.report_unused_imports(path, warnings);
        Ok(items)
    }

    /// Every alias this module declared that no reference ever looked up, now
    /// that its whole body is checked -- an alias used only inside a method
    /// body is exactly why this cannot run any earlier.
    fn report_unused_imports(&mut self, path: &[Ident], warnings: &mut TaggedWarnings) {
        for (alias, import) in &self.modules.index(path).imports {
            if self.imports.was_used(path, alias) {
                continue;
            }
            let kind = AnalysisWarningKind::UnusedImport { alias: alias.clone() };
            if import.suppress.iter().any(|s| s.as_ref() == kind.name()) {
                continue;
            }
            warnings.push((path.to_vec(), AnalysisWarning::new(import.id, import.span, kind)));
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
            let HirItem::FunctionDefinition(f) = &self.modules.parsed(module_path).hir.items[*index] else {
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
                    kind: ExternFunctionKind::Method { type_name: key.name.clone(), method_name },
                    mangling: method.annotations.mangling,
                    fn_type: method.fn_type,
                });
            }
        }

        functions
    }

    /// A free function's own resolved `@mangling(...)`, which a consuming
    /// compilation must agree with or the two mangled symbols diverge.
    fn mangling_of(&self, decl_id: &HirId) -> ManglingMode {
        self.items.function_annotations.get(decl_id).map(|a| a.mangling).unwrap_or_default()
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
    /// Scoped to local modules, matching every other end-of-compile sweep --
    /// an extern-owned type's "unused" status only reflects what *this*
    /// compilation happens to touch, not what a downstream consumer might, so
    /// warning on it would be a false positive by construction.
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
                AnalysisWarningKind::UnusedField { owner: owner.clone(), field: field.ident.clone() },
            )
        };

        for decl in group_by_declaration(self.items.cells.structs(), |c| (c.id, c.suppress.clone())) {
            let Some(def) = self.hir_struct(decl.module, decl.name) else { continue };
            if !local.contains(decl.module) || decl.suppresses("unused_field") {
                continue;
            }
            for (index, field) in def.fields.iter().enumerate() {
                if !decl.any(|id| usage.struct_fields.contains(&(id, index))) {
                    warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                }
            }
        }

        for decl in group_by_declaration(self.items.cells.unions(), |c| (c.id, c.suppress.clone())) {
            let Some(def) = self.hir_union(decl.module, decl.name) else { continue };
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
            let Some(def) = self.hir_enum(decl.module, decl.name) else { continue };
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
                        if !decl.any(|id| usage.enum_body_fields.contains(&(id, variant_index, field_index))) {
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
            let key = |path: &ModulePath| path.iter().map(|i| i.as_ref().to_string()).collect::<Vec<_>>();
            key(a_path).cmp(&key(b_path)).then(a.span.start.cmp(&b.span.start))
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
            .or_insert_with(|| Declaration { module: &key.module, name: &key.name, ids: vec![], suppress })
            .ids
            .push(id);
    }
    grouped.into_values().collect()
}
