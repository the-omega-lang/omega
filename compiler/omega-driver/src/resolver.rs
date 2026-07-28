//! What the analyzer asks the driver: everything module-tree-shaped.
//!
//! `omega-analyzer` never sees a filesystem or a cache -- it only ever asks
//! the questions on [`ModuleResolver`], and this is the one place they are
//! answered.

use crate::{Driver, ModulePath};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedMethod, ResolvedSpecType, ResolvedType};
use omega_analyzer::resolver::{
    GenericLiteralSignature, GenericSignature, ImportTarget, ItemNamespace, ModuleResolver, ResolveError,
    ResolvedItem,
};
use omega_analyzer::similarity::best_match;
use omega_hir::{HirId, HirItem};
use omega_parser::prelude::{Ident, Visibility};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// One import alias's resolution state -- the same white/gray/black cycle
/// guard items get, at `(module, alias)` granularity.
///
/// That granularity is what fixes a real false-cycle bug a whole-module
/// version of this guard used to have: resolving one item's context used to
/// require its *entire* module's import list resolved first, so two modules
/// whose *unrelated* items happened to cross-import each other would deadlock
/// on each other's whole list. Per-alias, only a name that genuinely,
/// directly needs itself still reports a cycle.
enum AliasState {
    InProgress,
    Done(Result<ImportTarget, ResolveError>),
}

/// Every import alias resolved so far, and every one anything ever asked for.
#[derive(Default)]
pub(crate) struct ImportState {
    resolved: HashMap<(ModulePath, Ident), AliasState>,
    /// The single choke point every alias use funnels through, so this is a
    /// complete record of "was this import ever actually used" by the time a
    /// module finishes body-checking -- what `UnusedImport` diffs against.
    used: HashSet<(ModulePath, Ident)>,
}

impl ImportState {
    fn mark_used(&mut self, module: &[Ident], alias: &Ident) {
        self.used.insert((module.to_vec(), alias.clone()));
    }

    pub fn was_used(&self, module: &[Ident], alias: &Ident) -> bool {
        self.used.contains(&(module.to_vec(), alias.clone()))
    }
}

impl Driver {
    /// `alias`'s resolved target in `module_path`, memoized and cycle-guarded
    /// per `(module, alias)` pair.
    fn resolve_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
        target: &[Ident],
        hidden: bool,
    ) -> Result<ImportTarget, ResolveError> {
        let key = (module_path.to_vec(), alias.clone());
        match self.imports.resolved.get(&key) {
            Some(AliasState::Done(result)) => return result.clone(),
            Some(AliasState::InProgress) => return Err(ResolveError::Cycle(vec![module_path.to_vec()])),
            None => {}
        }

        self.imports.resolved.insert(key.clone(), AliasState::InProgress);
        let result = self.resolve_import_target(module_path, target, hidden);
        self.imports.resolved.insert(key, AliasState::Done(result.clone()));
        result
    }

    /// What an already-absolute import path names: a real module (a pure
    /// filesystem check, no recursion), a generic item (deferred), or an
    /// ordinary item (eagerly resolved).
    fn resolve_import_target(
        &mut self,
        accessor: &[Ident],
        segments: &[Ident],
        hidden: bool,
    ) -> Result<ImportTarget, ResolveError> {
        match self.roots.locate(segments) {
            Ok(_) => return Ok(ImportTarget::Module(segments.to_vec())),
            // Real regardless of whether this turns out to be a whole-module
            // or an item import -- must surface here, not be masked by the
            // item-import fallback below.
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }

        let Some((item_name, module_path)) = segments.split_last() else {
            return Err(ResolveError::UnknownModule(segments.to_vec()));
        };

        // A *generic* item import supplies no type arguments at all (those
        // only ever appear at a use site), so eagerly instantiating here would
        // always fail with a spurious arg-count mismatch. This defers
        // entirely, carrying just the absolute path for the use site to
        // substitute in later.
        if self.is_generic_template(module_path, item_name)? {
            return Ok(ImportTarget::GenericItem(segments.to_vec()));
        }

        // Capturing "what does this alias refer to" never embeds anything
        // inline the way a struct field does -- always indirect here. The
        // absolute path travels along with the resolved snapshot so a
        // type-position consumer whose own `indirect` differs can re-resolve
        // with its own real value instead of trusting this one.
        let item = self.ensure_item(accessor, module_path, item_name, &[], true, hidden)?;
        Ok(ImportTarget::Item(segments.to_vec(), item))
    }

    /// One import alias's own structural facts, or `None` when the module
    /// binds no such alias.
    fn import_entry(&mut self, module_path: &[Ident], alias: &Ident) -> Result<Option<(ModulePath, bool)>, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        let Some(import) = self.modules.index(module_path).imports.get(alias) else {
            return Ok(None);
        };
        let entry = (import.target.clone(), import.hidden);
        // Querying an alias's target *is* using it, for `UnusedImport`'s
        // purposes, regardless of which query got here.
        self.imports.mark_used(module_path, alias);
        Ok(Some(entry))
    }
}

impl ModuleResolver for Driver {
    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        let Some((target, hidden)) = self.import_entry(module_path, alias)? else {
            return Ok(None);
        };
        self.resolve_alias(module_path, alias, &target, hidden).map(Some)
    }

    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident> {
        // Purely advisory (typo suggestions) -- a module that can't be indexed
        // reports its own failure elsewhere.
        if self.ensure_module_indexed(module_path).is_err() {
            return vec![];
        }
        self.modules.index(module_path).imports.keys().cloned().collect()
    }

    fn raw_import_absolute_path(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(ModulePath, bool)>, ResolveError> {
        self.import_entry(module_path, alias)
    }

    fn resolve_item(
        &mut self,
        accessor_module_path: &[Ident],
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        self.ensure_item(accessor_module_path, module_path, item_name, type_args, indirect, bypass)
    }

    /// Answered from the *declaration* rather than from any resolution cache:
    /// visibility is a property of how an item was declared, identical for
    /// every instantiation of a generic template, so no cache entry needs to
    /// exist (or be searched) to answer it. `false` for a name that doesn't
    /// resolve at all -- erring toward not claiming a bypass was unnecessary,
    /// rather than risking a wrong `UnnecessaryHidden` warning.
    fn is_item_visible(&mut self, accessor_module_path: &[Ident], absolute_path: &[Ident]) -> bool {
        let Some((item_name, module_path)) = absolute_path.split_last() else { return false };
        self.declared_visibility(module_path, item_name)
            .is_some_and(|visibility| Self::visibility_allows(visibility, module_path, accessor_module_path))
    }

    fn fresh_synthetic_id(&mut self) -> HirId {
        self.items.fresh_synthetic_id()
    }

    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError> {
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        // "Doesn't exist" is deferred to the ordinary call path, which
        // re-derives and reports it identically -- this query only ever needs
        // to say "not a generic function" either way.
        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let HirItem::FunctionDefinition(f) = &self.modules.parsed(module_path).hir.items[index] else {
            return Ok(None);
        };
        if f.generics.is_empty() {
            return Ok(None);
        }
        Ok(Some(GenericSignature {
            generics: f.generics.iter().map(|g| g.ident.clone()).collect(),
            params: f.params.iter().map(|p| p.r#type.clone()).collect(),
        }))
    }

    fn generic_literal_signature(
        &mut self,
        absolute_path: &[Ident],
        variant: Option<&Ident>,
    ) -> Result<Option<GenericLiteralSignature>, ResolveError> {
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        // "Doesn't exist"/"not generic" are deferred to the ordinary literal
        // path, which re-derives and reports them identically -- this query
        // only ever needs to say "not a generic literal target" either way.
        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let (generics, fields) = match (&self.modules.parsed(module_path).hir.items[index], variant) {
            (HirItem::Struct(s), None) => {
                (&s.generics, s.fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect())
            }
            (HirItem::Union(u), None) => {
                (&u.generics, u.fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect())
            }
            (HirItem::Enum(e), Some(variant_name)) => {
                let Some(v) = e.variants.iter().find(|v| &v.name == variant_name) else {
                    return Ok(None);
                };
                let fields = e
                    .dynamic_fields
                    .iter()
                    .chain(v.fields.iter())
                    .map(|f| (f.ident.clone(), f.r#type.clone()))
                    .collect();
                (&e.generics, fields)
            }
            _ => return Ok(None),
        };
        if generics.is_empty() {
            return Ok(None);
        }
        Ok(Some(GenericLiteralSignature { generics: generics.iter().map(|g| g.ident.clone()).collect(), fields }))
    }

    fn spec_declaration(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError> {
        self.resolve_spec_declaration(absolute_path)
    }

    fn function_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<(HirId, ResolvedFunctionType, Visibility)>>, ResolveError> {
        // A module-resolution failure here doesn't mean this call is broken --
        // it means `module_path` (the caller's naive "everything but the last
        // segment" split) isn't a real module at all, which is exactly what a
        // `Module::Type::function` static-call path looks like from here.
        // Swallowed for the same reason `generic_function_signature` swallows
        // it: "not a flat item path" just means "not this query's concern".
        if self.ensure_module_indexed(module_path).is_err() {
            return Ok(None);
        }
        let Some(indices) = self.modules.index(module_path).overloads.get(name).cloned() else {
            return Ok(None);
        };
        let hir = self.modules.hir(module_path);
        let mut candidates = Vec::with_capacity(indices.len());
        for index in indices {
            let HirItem::FunctionDefinition(f) = &hir.items[index] else {
                unreachable!("only a function is ever recorded as an overload candidate");
            };
            candidates.push((f.id, self.ensure_overload_signature(module_path, index)?, f.visibility));
        }
        Ok(Some(candidates))
    }

    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident> {
        // Purely advisory -- a module that can't even be indexed just produces
        // no suggestion (its own failure is reported elsewhere).
        if self.ensure_module_indexed(module_path).is_err() {
            return None;
        }
        let module = self.modules.get(module_path)?;
        let index = module.index.as_ref()?;
        let candidates = index
            .items
            .iter()
            .filter(|&(_, &i)| match &module.hir.items[i] {
                HirItem::Struct(_) | HirItem::Enum(_) | HirItem::Union(_) | HirItem::Spec(_) => {
                    namespace == ItemNamespace::Type
                }
                HirItem::FunctionDefinition(_) | HirItem::Declaration(_) | HirItem::ExternDeclaration(_) => {
                    namespace == ItemNamespace::Value
                }
                HirItem::Import(_) => false,
            })
            .map(|(name, _)| name);
        best_match(target, candidates)
    }

    fn extension_methods(&mut self, receiver: &ResolvedType) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError> {
        self.methods_attached_to(receiver)
    }
}
