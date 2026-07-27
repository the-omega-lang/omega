//! `for`-spec extensions: `spec Name : Deps for Target { ... }`, the only way
//! a primitive/slice type gets methods.
//!
//! Nothing ever imports a `for`-spec by name, so none of this is reachable
//! through the ordinary item query. Instead `core`'s own module tree is
//! walked once, lazily, the first time any receiver's methods are asked for,
//! and every `for` block found anywhere in it is registered here.

use crate::compile::CheckedModules;
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::analysis::{ExtensionTarget, PendingSpecMethod};
use omega_analyzer::checked::{CheckedFunctionDef, CheckedItem, CheckedModule};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind, AnalysisWarning};
use omega_analyzer::resolved_type::{ResolvedMethod, ResolvedType};
use omega_analyzer::resolver::ResolveError;
use omega_diagnostics::Span;
use omega_hir::{HirItem, HirSpecDef};
use omega_parser::prelude::Ident;
use std::collections::HashMap;

/// The one package `for`-spec extensions may be declared in. Anywhere else
/// they'd be silently unreachable (nothing walks a non-`core` tree looking
/// for them), so declaring one elsewhere is a hard error instead.
pub(crate) const CORE_MODULE: &str = "core";

/// Whether `path` belongs to the `core` package.
pub(crate) fn is_core_module(path: &[Ident]) -> bool {
    path.first().map(Ident::as_ref) == Some(CORE_MODULE)
}

/// One spec-default method body queued for a receiver, waiting to be checked.
/// Unlike a struct/enum/union's pending methods, this carries its own owning
/// module: a primitive receiver has no declared item of its own, so there's
/// no enclosing body-check call to supply one.
struct PendingExtension {
    module: ModulePath,
    method: PendingSpecMethod,
}

/// One checked extension method, ready to be merged into its owning `for`
/// spec's module.
struct CheckedExtension {
    module: ModulePath,
    function: CheckedFunctionDef,
    warnings: Vec<AnalysisWarning>,
}

/// Everything discovered about `for`-attached methods in this compilation.
#[derive(Default)]
pub(crate) struct Extensions {
    /// Whether `core`'s tree has already been walked -- nothing about its
    /// `for` declarations changes mid-compile, so this happens exactly once.
    discovered: bool,
    /// Set when the walk itself failed: `core` was registered (unlike the
    /// silently-tolerated "not registered at all" case) but something in its
    /// own tree is genuinely broken. Surfaced through the next lookup rather
    /// than swallowed.
    discovery_error: Option<ResolveError>,
    /// Every module the walk visited. Extends the scope errors are drained
    /// from, so a genuine error inside `core`'s own tree still surfaces even
    /// when nothing local imports that submodule. Deliberately *not* used to
    /// extend the warning scope -- `core`'s internal warnings shouldn't leak
    /// into every downstream compilation that merely uses one of its methods.
    pub module_paths: Vec<ModulePath>,
    /// The one `[T]`-pattern `for` block, if any -- kept raw, to be resolved
    /// per-receiver on demand, since there's no single instantiation to
    /// resolve eagerly the way a concrete target has.
    pattern: Option<(ModulePath, HirSpecDef)>,
    /// Every receiver's already-resolved, flattened method list. Keying on a
    /// `ResolvedType` is sound despite its interior mutability: every cell it
    /// can contain hashes and compares by its own `id` alone, which is
    /// decided once at creation and never patched afterwards.
    ///
    /// A concrete
    /// target's entry is filled in eagerly by the walk (there's exactly one
    /// possible receiver, ever); a pattern target's entries are filled in
    /// lazily, one per distinct concrete receiver (`[i32]` and `[u8]` cached
    /// separately, from the one spec).
    resolved: HashMap<ResolvedType, Vec<(Ident, ResolvedMethod)>>,
    /// Default bodies queued per receiver, not yet checked -- the extension
    /// counterpart of the pending spec methods a struct/enum/union queues.
    /// `IndexMap` so draining visits receivers in the order they were first
    /// resolved, keeping repeated builds byte-for-byte identical.
    pending: IndexMap<ResolvedType, Vec<PendingExtension>>,
}

impl Driver {
    /// Walks `core`'s entire module tree, registering every `spec ... for
    /// Target { ... }` declared anywhere in it. Memoized.
    ///
    /// A concrete target is fully resolved right here (there's exactly one
    /// possible receiver for it); the one `[T]`-pattern target found, if any,
    /// is stashed for per-receiver resolution later. "Only one `for` block
    /// per target type" is enforced here, once, across the whole tree.
    ///
    /// Does nothing (not an error) when `core` isn't registered for this
    /// compilation at all -- `for`-attached methods are simply unavailable,
    /// like any other `--extern`-gated feature.
    fn discover_extensions(&mut self) {
        if self.extensions.discovered {
            return;
        }
        self.extensions.discovered = true;

        let core = vec![Ident(CORE_MODULE.to_string())];
        if !self.roots.module_exists(&core) {
            return;
        }
        let module_paths = match self.discover_module_tree(&core) {
            Ok(paths) => paths,
            Err(error) => {
                self.extensions.discovery_error = Some(error);
                return;
            }
        };
        self.extensions.module_paths = module_paths.clone();

        // See `Extensions::resolved` on why a `ResolvedType` key is stable.
        #[allow(clippy::mutable_key_type)]
        let mut concrete_sites: HashMap<ResolvedType, Span> = HashMap::new();
        let mut pattern_site: Option<Span> = None;

        for module_path in module_paths {
            let hir = self.modules.hir(&module_path);
            let specs = hir.items.iter().filter_map(|item| match item {
                HirItem::Spec(sp) if sp.target.is_some() => Some(sp.clone()),
                _ => None,
            });
            for sp in specs.collect::<Vec<_>>() {
                let owner = (sp.id, sp.span);
                let target = self.analyze(&module_path, &[], owner, |a| a.resolve_extension_target(&sp));
                match target {
                    Some(ExtensionTarget::Concrete(receiver)) => {
                        match concrete_sites.get(&receiver) {
                            Some(previous) => self.duplicate_target(&module_path, &sp, &receiver.to_string(), *previous),
                            None => {
                                concrete_sites.insert(receiver.clone(), sp.span);
                                let self_type = receiver.clone();
                                self.resolve_for_block(&module_path, &sp, &receiver, self_type, None);
                            }
                        }
                    }
                    Some(ExtensionTarget::Pattern) => match pattern_site {
                        Some(previous) => self.duplicate_target(&module_path, &sp, "[T]", previous),
                        None => {
                            pattern_site = Some(sp.span);
                            self.extensions.pattern = Some((module_path.clone(), sp));
                        }
                    },
                    None => {}
                }
            }
        }
    }

    fn duplicate_target(&mut self, module_path: &[Ident], sp: &HirSpecDef, target: &str, previous: Span) {
        self.diagnostics.error(
            module_path,
            AnalysisError::new(
                sp.id,
                sp.span,
                AnalysisErrorKind::DuplicateExtensionTarget { target: target.to_string(), previous },
            ),
        );
    }

    /// Resolves one `for` block's methods for one concrete receiver, caching
    /// the flattened list and queueing every default body it brought along.
    ///
    /// `self_type` is what `Self` binds to inside the block, which is *not*
    /// always `receiver`: for the `[T]` pattern it's the bare `[T]` shape
    /// (`Array`), so that a `*self` param's existing `Pointer(Array(_)) ->
    /// Slice` resolution is what turns it back into the real, lengthed
    /// receiver instead of double-wrapping it. `pattern_binding` is the
    /// concrete `T` that pattern's element type binds to, `None` for a
    /// concrete target.
    fn resolve_for_block(
        &mut self,
        module_path: &[Ident],
        sp: &HirSpecDef,
        receiver: &ResolvedType,
        self_type: ResolvedType,
        pattern_binding: Option<ResolvedType>,
    ) -> Vec<(Ident, ResolvedMethod)> {
        let resolved = self.analyze(module_path, &[], (sp.id, sp.span), |analyzer| {
            analyzer.resolve_extension_methods(sp, &self_type, pattern_binding)
        });

        let (methods, pending, _implemented_specs) = resolved.unwrap_or_default();
        self.extensions.resolved.insert(receiver.clone(), methods.clone());
        if !pending.is_empty() {
            let pending = pending
                .into_iter()
                .map(|method| PendingExtension { module: module_path.to_vec(), method })
                .collect();
            self.extensions.pending.insert(receiver.clone(), pending);
        }
        methods
    }

    /// `receiver`'s `for`-attached methods (see
    /// `ModuleResolver::extension_methods`), discovering `core`'s tree on
    /// first use and memoizing per receiver afterwards.
    pub(crate) fn methods_attached_to(
        &mut self,
        receiver: &ResolvedType,
    ) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError> {
        // Returns immediately once the walk has already run, so this is also
        // the memoized path's only cost.
        self.discover_extensions();
        if let Some(error) = &self.extensions.discovery_error {
            return Err(error.clone());
        }
        // A concrete target was resolved by the walk itself, and every
        // pattern receiver asked about before is cached -- so a miss here
        // means either nothing targets `receiver` at all, or it's a fresh job
        // for the one `[T]`-pattern block, matched below.
        if let Some(methods) = self.extensions.resolved.get(receiver) {
            return Ok(methods.clone());
        }
        let Some((module_path, sp)) = self.extensions.pattern.clone() else {
            return Ok(Vec::new());
        };
        // Only a slice has a real element type to bind the pattern's `T` to
        // -- callers only ever hand this a primitive/slice/str, already
        // deref'd at most once.
        let ResolvedType::Slice { item, .. } = receiver else {
            return Ok(Vec::new());
        };
        let element = (**item).clone();
        Ok(self.resolve_for_block(&module_path, &sp, receiver, ResolvedType::Array(item.clone()), Some(element)))
    }

    /// Checks every extension method body queued for `receiver`, force-
    /// seeding `type_args`/`extension_target` so codegen mangles and links
    /// each one correctly (which applies even to a non-generic, concrete
    /// receiver -- see `CheckedFunctionDef::extension_target`).
    fn check_pending_extensions(&mut self, receiver: &ResolvedType) -> Vec<CheckedExtension> {
        let pending = self.extensions.pending.shift_remove(receiver).unwrap_or_default();
        let mut checked = Vec::with_capacity(pending.len());
        for PendingExtension { module, method } in pending {
            let run = self.with_analyzer(&module, &method.substitution, (method.id, method.raw.span), |a| {
                a.check_pending_spec_method(&method)
            });
            if let Some(mut function) = run.result {
                function.type_args = vec![receiver.clone()];
                function.extension_target = Some(receiver.clone());
                checked.push(CheckedExtension { module, function, warnings: run.warnings });
            }
        }
        checked
    }

    /// Drains every extension method body queued anywhere, merging each into
    /// its owning module -- creating a fresh, empty entry first if that
    /// module was never itself import-reachable (the whole point of `for`:
    /// nothing needs to import a `for` block's module to use what it
    /// attaches).
    ///
    /// A `while let`, not a single `for`, because checking one pending body
    /// can itself discover and queue methods for a *different* receiver
    /// mid-drain.
    pub(crate) fn drain_pending_extensions(
        &mut self,
        modules: &mut CheckedModules,
        warnings: &mut Vec<(ModulePath, AnalysisWarning)>,
    ) {
        while let Some(receiver) = self.extensions.pending.keys().next().cloned() {
            for extension in self.check_pending_extensions(&receiver) {
                let CheckedExtension { module, function, warnings: item_warnings } = extension;
                warnings.extend(item_warnings.into_iter().map(|w| (module.clone(), w)));
                let item = CheckedItem::FunctionDefinition(function);
                match modules.iter_mut().find(|(path, _)| *path == module) {
                    Some((_, checked_module)) => checked_module.items.push(item),
                    None => {
                        let id = self.modules.parsed(&module).id;
                        modules.push((module, CheckedModule { id, items: vec![item] }));
                    }
                }
            }
        }
    }

    /// Rejects a `for` block declared outside `core`, where nothing would
    /// ever discover it. Checked per module during `compile`, since the
    /// ordinary per-item sweep skips `for` blocks entirely (they have no
    /// name to be reached by).
    pub(crate) fn reject_local_extensions(&mut self, path: &[Ident]) {
        if is_core_module(path) {
            return;
        }
        let hir = self.modules.hir(path);
        for item in &hir.items {
            let HirItem::Spec(sp) = item else { continue };
            if sp.target.is_some() {
                self.diagnostics.error(
                    path,
                    AnalysisError::new(
                        sp.id,
                        sp.span,
                        AnalysisErrorKind::ExtensionOutsideCore { name: sp.name.clone() },
                    ),
                );
            }
        }
    }
}
