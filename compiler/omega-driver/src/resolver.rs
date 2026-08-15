//! What the analyzer asks the driver: everything module-tree-shaped.
//!
//! `omega-analyzer` never sees a filesystem or a cache -- it only ever asks
//! the questions on [`ModuleResolver`], and this is the one place they are
//! answered.

use crate::{Driver, ModulePath};
use omega_analyzer::analysis::item_visibility;
use omega_analyzer::checked::{CheckedFunctionDef, CheckedItem};
use omega_analyzer::resolved_type::{
    ConstValue, ResolvedConformance, ResolvedFunctionType, ResolvedMethod, ResolvedSpecType,
    ResolvedType,
};
use omega_analyzer::resolver::{
    GenericLiteralSignature, GenericSignature, GenericStaticFunctionSignature, ImportTarget,
    ItemNamespace, ModuleResolver, ResolveError, ResolvedItem,
};
use omega_analyzer::similarity::best_match;
use omega_hir::{HirFunctionDef, HirGenericParam, HirId, HirItem};
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
        reveal: bool,
    ) -> Result<ImportTarget, ResolveError> {
        let key = (module_path.to_vec(), alias.clone());
        match self.imports.resolved.get(&key) {
            Some(AliasState::Done(result)) => return result.clone(),
            Some(AliasState::InProgress) => {
                return Err(ResolveError::Cycle(vec![module_path.to_vec()]));
            }
            None => {}
        }

        self.imports
            .resolved
            .insert(key.clone(), AliasState::InProgress);
        let result = self.resolve_import_target(module_path, target, reveal);
        self.imports
            .resolved
            .insert(key, AliasState::Done(result.clone()));
        result
    }

    /// What an already-absolute import path names: a real module (a pure
    /// filesystem check, no recursion), a generic item (deferred), or an
    /// ordinary item (eagerly resolved).
    fn resolve_import_target(
        &mut self,
        accessor: &[Ident],
        segments: &[Ident],
        reveal: bool,
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
        let item = self.ensure_item(accessor, module_path, item_name, &[], true, reveal)?;
        Ok(ImportTarget::Item(segments.to_vec(), item))
    }

    /// One import alias's own structural facts, or `None` when the module
    /// binds no such alias.
    fn import_entry(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(ModulePath, bool)>, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        let Some(import) = self.modules.index(module_path).imports.get(alias) else {
            return Ok(None);
        };
        let entry = (import.target.clone(), import.reveal);
        // Querying an alias's target *is* using it, for `UnusedImport`'s
        // purposes, regardless of which query got here.
        self.imports.mark_used(module_path, alias);
        Ok(Some(entry))
    }
}

impl ModuleResolver for Driver {
    fn macro_origin_module(&self, origin: omega_parser::prelude::Origin) -> Option<Vec<Ident>> {
        self.modules.macro_origin_module(origin)
    }

    fn macro_origin_visibility(
        &self,
        origin: omega_parser::prelude::Origin,
    ) -> Option<Visibility> {
        self.modules.macro_origin_visibility(origin)
    }

    fn declared_item_visibility(&mut self, absolute_path: &[Ident]) -> Option<Visibility> {
        let (name, module) = absolute_path.split_last()?;
        self.declared_visibility(module, name)
    }

    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        let Some((target, reveal)) = self.import_entry(module_path, alias)? else {
            // No explicit `import` binds this alias -- `core` is always
            // implicitly available as a qualified-path prefix anyway (see
            // `docs/10-modules-and-linkage.md`'s "core is a prelude"
            // section), except from within `core`'s own tree, which still
            // needs real imports among its own submodules like anything
            // else. `Item`/`GenericItem` targets never apply here -- an
            // implicit import always names the whole `core` module, never
            // one specific item inside it.
            if alias.as_ref() == crate::roots::CORE_MODULE
                && !crate::roots::is_core_module(module_path)
                && !self.roots.core_modules().is_empty()
            {
                return Ok(Some(ImportTarget::Module(vec![Ident(
                    crate::roots::CORE_MODULE.to_string(),
                )])));
            }
            return Ok(None);
        };
        self.resolve_alias(module_path, alias, &target, reveal)
            .map(Some)
    }

    fn ambient_core_candidates(
        &mut self,
        accessor: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<Ident>>, ResolveError> {
        if crate::roots::is_core_module(accessor) {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        for path in self.roots.core_modules() {
            // Best-effort: a broken core module has its own real error
            // recorded wherever *it* is actually checked (`core`'s own
            // build, or whatever local code imports it directly) -- an
            // unrelated bare-name lookup elsewhere shouldn't also surface
            // it a second time, so it's just skipped here.
            if self.ensure_module_indexed(&path).is_err() {
                continue;
            }
            let index = self.modules.index(&path);
            if index.overloads.contains_key(name) {
                continue; // scope cut -- see `ModuleResolver::ambient_core_candidates`'s doc comment
            }
            let Some(&item_index) = index.items.get(name) else {
                continue;
            };
            let item = &self.modules.parsed(&path).hir.items[item_index];
            if item_visibility(item) == Visibility::Exposed {
                candidates.push(
                    path.iter()
                        .cloned()
                        .chain(std::iter::once(name.clone()))
                        .collect(),
                );
            }
        }
        match candidates.len() {
            0 => Ok(None),
            1 => Ok(Some(candidates.pop().unwrap())),
            _ => Err(ResolveError::AmbiguousAmbientName {
                name: name.clone(),
                candidates,
            }),
        }
    }

    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident> {
        // Purely advisory (typo suggestions) -- a module that can't be indexed
        // reports its own failure elsewhere.
        if self.ensure_module_indexed(module_path).is_err() {
            return vec![];
        }
        self.modules
            .index(module_path)
            .imports
            .keys()
            .cloned()
            .collect()
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
        self.ensure_item(
            accessor_module_path,
            module_path,
            item_name,
            type_args,
            indirect,
            bypass,
        )
    }

    /// Answered from the *declaration* rather than from any resolution cache:
    /// visibility is a property of how an item was declared, identical for
    /// every instantiation of a generic template, so no cache entry needs to
    /// exist (or be searched) to answer it. `false` for a name that doesn't
    /// resolve at all -- erring toward not claiming a bypass was unnecessary,
    /// rather than risking a wrong `UnnecessaryReveal` warning.
    fn is_item_visible(&mut self, accessor_module_path: &[Ident], absolute_path: &[Ident]) -> bool {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return false;
        };
        self.declared_visibility(module_path, item_name)
            .is_some_and(|visibility| {
                Self::visibility_allows(visibility, module_path, accessor_module_path)
            })
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
        let HirItem::FunctionDefinition(f) = &self.modules.parsed(module_path).hir.items[index]
        else {
            return Ok(None);
        };
        if f.generics.is_empty() {
            return Ok(None);
        }
        Ok(Some(GenericSignature {
            generics: f.generics.iter().map(|g| g.ident.clone()).collect(),
            defaults: f.generics.iter().map(|g| g.default.clone()).collect(),
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
        let (generics, fields) = match (&self.modules.parsed(module_path).hir.items[index], variant)
        {
            (HirItem::Struct(s), None) => (
                &s.generics,
                s.fields
                    .iter()
                    .map(|f| (f.ident.clone(), f.r#type.clone()))
                    .collect(),
            ),
            (HirItem::Union(u), None) => (
                &u.generics,
                u.fields
                    .iter()
                    .map(|f| (f.ident.clone(), f.r#type.clone()))
                    .collect(),
            ),
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
        Ok(Some(GenericLiteralSignature {
            generics: generics.iter().map(|g| g.ident.clone()).collect(),
            defaults: generics.iter().map(|g| g.default.clone()).collect(),
            fields,
        }))
    }

    fn generic_static_function_signature(
        &mut self,
        owner_absolute: &[Ident],
        function_name: &Ident,
    ) -> Result<Option<GenericStaticFunctionSignature>, ResolveError> {
        let Some((name, module_path)) = owner_absolute.split_last() else {
            return Err(ResolveError::UnknownModule(owner_absolute.to_vec()));
        };
        // "Doesn't exist"/"not generic"/"no such static function" are all
        // deferred to the ordinary call path, which re-derives and reports
        // them identically -- this query only ever needs to say "not this
        // shape" either way.
        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let (owner_generics, functions): (&[HirGenericParam], &[HirFunctionDef]) =
            match &self.modules.parsed(module_path).hir.items[index] {
                HirItem::Struct(s) => (&s.generics, &s.functions),
                HirItem::Union(u) => (&u.generics, &u.functions),
                HirItem::Enum(e) => (&e.generics, &e.functions),
                _ => return Ok(None),
            };
        if owner_generics.is_empty() {
            return Ok(None);
        }
        // Exactly one candidate only -- 2+ overloaded statics under this
        // name is `resolve_overloaded_static_call`'s own concern (once the
        // owner is concrete), not this query's; composing overload scoring
        // with owner-generic inference at once is deliberately out of
        // scope (see `Analyzer::resolve_generic_static_call`'s doc
        // comment), so this must not silently pick a first match.
        let mut matches = functions
            .iter()
            .filter(|f| &f.name == function_name && f.self_mode.is_none());
        let f = matches.next();
        if matches.next().is_some() {
            return Ok(None);
        }
        let Some(f) = f else {
            return Ok(None);
        };
        Ok(Some(GenericStaticFunctionSignature {
            owner_generics: owner_generics.iter().map(|g| g.ident.clone()).collect(),
            owner_defaults: owner_generics.iter().map(|g| g.default.clone()).collect(),
            function_generics: f.generics.iter().map(|g| g.ident.clone()).collect(),
            params: f.params.iter().map(|p| p.r#type.clone()).collect(),
        }))
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
            candidates.push((
                f.id,
                self.ensure_overload_signature(module_path, index)?,
                f.visibility,
            ));
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
                HirItem::Gap(_) => namespace == ItemNamespace::Value,
                HirItem::FunctionDefinition(_)
                | HirItem::Declaration(_)
                | HirItem::DeclarationWithInit(..)
                | HirItem::Walrus(_)
                | HirItem::ExternDeclaration(_) => namespace == ItemNamespace::Value,
                HirItem::Glue(_)
                | HirItem::Conform(_)
                | HirItem::Primitive(_)
                | HirItem::Import(_) => false,
            })
            .map(|(name, _)| name);
        best_match(target, candidates)
    }

    fn primitive_methods(
        &mut self,
        receiver: &ResolvedType,
    ) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError> {
        Ok(Driver::primitive_methods(self, receiver))
    }

    fn conformance_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Result<Option<ResolvedConformance>, ResolveError> {
        Ok(
            Driver::conformance_for(self, target, spec, spec_args).map(|entry| ResolvedConformance {
                target: entry.target,
                spec: entry.spec,
                spec_args: entry.spec_args,
                methods: entry.methods,
            }),
        )
    }

    fn conformances_for_type(
        &mut self,
        target: &ResolvedType,
    ) -> Result<Vec<ResolvedConformance>, ResolveError> {
        Ok(Driver::conformances_for_type(self, target)
            .into_iter()
            .map(|entry| ResolvedConformance {
                target: entry.target,
                spec: entry.spec,
                spec_args: entry.spec_args,
                methods: entry.methods,
            })
            .collect())
    }

    /// `decl_id`'s owning item, found via `items.decl_id_owner` (populated
    /// by `ItemQueries::identity_for`, the one place a function/method's
    /// identity is ever decided) then body-checked (or served from cache)
    /// through `ensure_item_body` -- see its own doc comment for why this,
    /// unlike every other query above, can run before `compile`'s ordinary
    /// phase-2 sweep would otherwise have reached this item, and why that's
    /// safe. A `decl_id` with no entry in `decl_id_owner` at all is
    /// impossible for a real `comp` call in practice (see `comp_eval::
    /// Interpreter::eval_call`'s own guard: it only ever calls this with a
    /// `Storage::Function` place's `decl_id`, which `compute_item` always
    /// records identity for) -- treated as `Ok(None)` rather than a panic
    /// regardless, since nothing here can prove that guard holds.
    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
        let Some(key) = self.items.decl_id_owner.get(&decl_id).cloned() else {
            return Ok(None);
        };
        let index = self.local_item_index(&key.module, &key.name)?;
        let Some(body) = self.ensure_item_body(&key, index) else {
            return Err(ResolveError::ItemFailed {
                module: key.module,
                item: key.name,
            });
        };
        Ok(match body.item {
            // The ordinary case: `key` names the free function directly,
            // and `identity_for` guarantees its own id is exactly `decl_id`.
            CheckedItem::FunctionDefinition(f) => Some(f),
            // `key` names the *owning* struct/enum/union -- a method has no
            // `ItemKey` of its own (see `decl_id_owner`'s doc comment), so
            // its body is found by searching its owner's already-checked
            // method list instead.
            CheckedItem::Struct(s) => s.functions.into_iter().find(|f| f.id == decl_id),
            CheckedItem::Enum(e) => e.functions.into_iter().find(|f| f.id == decl_id),
            CheckedItem::Union(u) => u.functions.into_iter().find(|f| f.id == decl_id),
            // Neither ever gets a `Storage::Function` place root, so a real
            // `comp` call can't actually reach this arm.
            CheckedItem::Declaration(_) | CheckedItem::ExternDeclaration(_) => None,
        })
    }

    /// A plain lookup into `items.comp_values` -- unlike `resolve_function_
    /// body`, nothing here can be "not yet computed but computable on
    /// demand": a top-level `comp` binding's value is always produced
    /// eagerly, during its own signature resolution (`compute_item`'s
    /// `Walrus` arm), and `decl_id` only ever reaches this call at all
    /// once the binding it names has already resolved successfully (its
    /// own reference went through the ordinary `ModuleResolver::
    /// resolve_item` -> `Storage::Comp` path first).
    fn resolve_comp_value(&mut self, decl_id: HirId) -> Option<ConstValue> {
        self.items.comp_values.get(&decl_id).cloned()
    }
}
