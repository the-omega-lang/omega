use crate::{Driver, ModulePath};
use omega_analyzer::analysis::item_visibility;
use omega_analyzer::checked::{CheckedFunctionDef, CheckedItem};
use omega_analyzer::resolved_type::{
    ConstValue, FunctionNamespace, ResolvedConformance, ResolvedMethod, ResolvedSpecType,
    ResolvedType,
};
use omega_analyzer::resolver::{
    GenericLiteralSignature, GenericOwnerFunctionSignature, GenericSignature, ImportTarget,
    ItemAccess, ItemNamespace, ModuleResolver, OverloadCandidate, OverloadCandidates, ResolveError,
    ResolveItemOptions, ResolvedAlias, ResolvedItem, ResolvedOverloadSet,
};
use omega_analyzer::similarity::best_match;
use omega_hir::{HirFunctionDef, HirGenericParam, HirId, HirItem};
use omega_parser::prelude::{FunctionType, Ident, Path, Type, Visibility};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

type AliasKey = (ModulePath, Ident);

enum AliasState {
    InProgress,
    Resolved(ImportTarget),
    Failed(ResolveError),
}

#[derive(Default)]
pub(crate) struct ImportState {
    resolved: HashMap<AliasKey, AliasState>,
    used: HashSet<AliasKey>,
    resolution_stack: Vec<AliasKey>,
}

impl ImportState {
    fn begin_resolution(&mut self, key: &AliasKey) {
        self.resolved.insert(key.clone(), AliasState::InProgress);
        self.resolution_stack.push(key.clone());
    }

    fn finish_resolution(&mut self, key: &AliasKey, result: Result<ImportTarget, ResolveError>) {
        let active = self
            .resolution_stack
            .pop()
            .expect("finishing an import resolution requires an active query");
        assert_eq!(&active, key, "query stack must unwind in LIFO order");
        let state = match result {
            Ok(target) => AliasState::Resolved(target),
            Err(error) => AliasState::Failed(error),
        };
        self.resolved.insert(key.clone(), state);
    }

    fn cycle_path(&self, key: &AliasKey) -> Vec<ModulePath> {
        let start = self
            .resolution_stack
            .iter()
            .position(|active| active == key)
            .expect("an in-progress query must be present in the resolution stack");
        self.resolution_stack[start..]
            .iter()
            .chain(std::iter::once(key))
            .map(|(module, alias)| {
                let mut path = module.clone();
                path.push(alias.clone());
                path
            })
            .collect()
    }

    fn mark_used(&mut self, module: &[Ident], alias: &Ident) {
        self.used.insert((module.to_vec(), alias.clone()));
    }

    pub fn was_used(&self, module: &[Ident], alias: &Ident) -> bool {
        self.used.contains(&(module.to_vec(), alias.clone()))
    }
}

impl Driver {
    fn resolve_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
        target: &[Ident],
        reveal: bool,
    ) -> Result<ImportTarget, ResolveError> {
        let key = (module_path.to_vec(), alias.clone());
        match self.imports.resolved.get(&key) {
            Some(AliasState::Resolved(target)) => return Ok(target.clone()),
            Some(AliasState::Failed(error)) => return Err(error.clone()),
            Some(AliasState::InProgress) => {
                return Err(ResolveError::Cycle(self.imports.cycle_path(&key)));
            }
            None => {}
        }

        self.imports.begin_resolution(&key);
        let result = self.resolve_import_target(module_path, target, reveal);
        self.imports.finish_resolution(&key, result.clone());
        result
    }

    fn resolve_import_target(
        &mut self,
        accessor: &[Ident],
        segments: &[Ident],
        reveal: bool,
    ) -> Result<ImportTarget, ResolveError> {
        match self.roots.locate(segments) {
            Ok(_) => return Ok(ImportTarget::Module(segments.to_vec())),
            // Real either way -- must surface here, not be masked by an
            // alias/module-binding or item-import fallback below.
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }
        // A module alias is a module binding, so imports may traverse one in
        // exactly the same places an ordinary path can: `import api::Thing`
        // must not require first importing `api` into a second statement.
        if let Some(module) = self.resolve_module_path(accessor, segments)? {
            return Ok(ImportTarget::Module(module));
        }

        let Some((item_name, written_module_path)) = segments.split_last() else {
            return Err(ResolveError::UnknownModule(segments.to_vec()));
        };
        let item_name = item_name.clone();
        let module_path = self
            .resolve_module_path(accessor, written_module_path)?
            .unwrap_or_else(|| written_module_path.to_vec());
        let absolute: Vec<Ident> = module_path
            .iter()
            .cloned()
            .chain(std::iter::once(item_name.clone()))
            .collect();

        // A declared alias binds a name the same way a generic template does:
        // its target resolution is lazy, but the import eagerly passes the
        // alias's own visibility gate. `reveal` is retained on the lazy
        // access because later consumers must not silently revoke the
        // capability established by this import.
        if self.alias_index(&module_path, &item_name)?.is_some() {
            let target = self
                .visible_alias(accessor, &module_path, &item_name, reveal)?
                .expect("just indexed by alias_index");
            if let ResolvedAlias::Module(module) = target {
                return Ok(ImportTarget::Module(module));
            }
            return Ok(ImportTarget::ItemPath(ItemAccess {
                absolute,
                bypass_visibility: reveal,
            }));
        }

        // An overload group is likewise lazy: which candidate a name means
        // is a use-site question, and the candidates may differ in
        // visibility. The import still gates eagerly on there being at least
        // one candidate this accessor can name.
        if !reveal {
            self.visible_overload_decls(accessor, &module_path, &item_name, false)?;
        }
        if self
            .modules
            .index(&module_path)
            .overloads
            .contains_key(&item_name)
            || self.is_generic_template(&module_path, &item_name)?
        {
            return Ok(ImportTarget::ItemPath(ItemAccess {
                absolute,
                bypass_visibility: reveal,
            }));
        }

        let item = self.ensure_item(
            accessor,
            &module_path,
            &item_name,
            &[],
            ResolveItemOptions::INDIRECT.bypassing_visibility(reveal),
        )?;
        Ok(ImportTarget::Item(absolute, item))
    }

    /// Import-alias lookup only. Declared aliases deliberately do not enter
    /// here, so alias-target resolution can consult imports without looping
    /// back through the alias query.
    pub(crate) fn resolve_import_alias_entry(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        self.resolve_import_alias_binding(module_path, alias, true)
            .map(|binding| binding.map(|(target, _)| target))
    }

    /// The same lookup without consuming the import. Alias declaration
    /// validation uses this form and only marks dependencies after the whole
    /// alias has resolved successfully, so a rejected declaration cannot make
    /// an otherwise-unused import appear used.
    pub(crate) fn peek_import_alias_binding(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(ImportTarget, bool)>, ResolveError> {
        self.resolve_import_alias_binding(module_path, alias, false)
    }

    fn resolve_import_alias_binding(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
        mark_used: bool,
    ) -> Result<Option<(ImportTarget, bool)>, ResolveError> {
        let entry = if mark_used {
            self.import_entry(module_path, alias)?
        } else {
            self.peek_import_entry(module_path, alias)?
        };
        let Some((target, reveal)) = entry else {
            if alias.as_ref() == crate::roots::CORE_MODULE
                && !crate::roots::is_core_module(module_path)
                && !self.roots.core_modules().is_empty()
            {
                return Ok(Some((
                    ImportTarget::Module(vec![Ident(crate::roots::CORE_MODULE.to_string())]),
                    false,
                )));
            }
            return Ok(None);
        };
        self.resolve_alias(module_path, alias, &target, reveal)
            .map(|target| Some((target, reveal)))
    }

    pub(crate) fn import_entry(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(ModulePath, bool)>, ResolveError> {
        let entry = self.peek_import_entry(module_path, alias)?;
        if entry.is_some() {
            self.imports.mark_used(module_path, alias);
        }
        Ok(entry)
    }

    pub(crate) fn peek_import_entry(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(ModulePath, bool)>, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        let Some(import) = self.modules.index(module_path).imports.get(alias) else {
            return Ok(None);
        };
        Ok(Some((import.target.clone(), import.reveal)))
    }
}

impl Driver {
    /// Every candidate declared under `name`, with no visibility filtering:
    /// the source `resolve_overload_set` gates. Nothing else should consume
    /// it, since an unfiltered candidate set is not an answer to any
    /// caller's question.
    fn raw_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<OverloadCandidates>, ResolveError> {
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
            candidates.push(OverloadCandidate {
                decl_id: f.id,
                fn_type: self.ensure_overload_signature(module_path, index)?,
                visibility: f.visibility,
            });
        }
        Ok(Some(candidates))
    }

    /// The absolute path a query should really answer for. A plain-path alias
    /// forwards to its target's own path so every downstream query keeps
    /// working on the target's identity; anything else answers for itself.
    fn canonical_query_path(&mut self, absolute_path: &[Ident]) -> ModulePath {
        let Some((name, module)) = absolute_path.split_last() else {
            return absolute_path.to_vec();
        };
        match self.declared_alias(module, name) {
            Ok(Some(ResolvedAlias::Item(target))) => target,
            Ok(Some(ResolvedAlias::Overloads { absolute, .. })) => absolute,
            _ => absolute_path.to_vec(),
        }
    }

    /// Re-export in one place: the caller is gated on the alias, and the
    /// target is then reached with the alias declaration module's own rights.
    fn resolve_through_alias(
        &mut self,
        accessor: &[Ident],
        alias_module: &[Ident],
        alias_name: &Ident,
        target: ResolvedAlias,
        type_args: &[ResolvedType],
        options: ResolveItemOptions,
    ) -> Result<ResolvedItem, ResolveError> {
        let visibility = self
            .alias_visibility(alias_module, alias_name)
            .expect("the alias was just resolved from this module");
        if !options.bypasses_visibility()
            && !Self::visibility_allows(visibility, alias_module, accessor)
        {
            return Err(ResolveError::NotVisible {
                module: alias_module.to_vec(),
                item: alias_name.clone(),
            });
        }
        match target {
            ResolvedAlias::Module(path) => Err(ResolveError::UnknownItem {
                module: path,
                item: alias_name.clone(),
            }),
            ResolvedAlias::Overloads { absolute: path, .. } | ResolvedAlias::Item(path) => {
                let (name, module) = path
                    .split_last()
                    .expect("an alias item target is never empty");
                // `path` is already the end of a fully-validated chain: every
                // intermediate alias's own declaration-site visibility to its
                // own immediate target was checked when the chain was built
                // (see `forward_alias_path`), so the final hop's target is
                // necessarily visible from its own containing module already.
                // Re-checking it against `alias_module` -- the *originally
                // named* alias's declaration module, which can differ from
                // this target's own module in a multi-hop chain -- would
                // reject a legitimately re-exported cross-module chain.
                self.ensure_item(
                    alias_module,
                    module,
                    name,
                    type_args,
                    options.bypassing_visibility(true),
                )
            }
            ResolvedAlias::Type { generics, r#type } => {
                self.resolve_alias_type(alias_module, alias_name, &generics, &r#type, type_args)
            }
        }
    }

    /// Resolves a structural alias target reached through item resolution
    /// rather than through type-syntax expansion -- `Alias::static_fn()` and
    /// `Alias { .. }` name a type without ever passing a `Type` down.
    fn resolve_alias_type(
        &mut self,
        alias_module: &[Ident],
        alias_name: &Ident,
        generics: &[HirGenericParam],
        r#type: &Type,
        type_args: &[ResolvedType],
    ) -> Result<ResolvedItem, ResolveError> {
        let index = self
            .alias_index(alias_module, alias_name)?
            .expect("the alias was just resolved from this module");
        // Alias generic defaults are bound by the same arity/default rule
        // ordinary item/aggregate-construction positions use, so a defaulted
        // alias argument works here exactly as it does in a plain type
        // position.
        let type_args =
            self.pad_generic_defaults(alias_module, alias_name, index, generics, type_args)?;
        let type_args = type_args.as_slice();
        let owner = omega_analyzer::analysis::item_site(
            &self.modules.parsed(alias_module).hir.items[index],
        );
        let substitution: Vec<(Ident, ResolvedType)> = generics
            .iter()
            .map(|g| g.ident.clone())
            .zip(type_args.iter().cloned())
            .collect();
        match self.check_generic_bounds(alias_module, owner, generics, type_args) {
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error),
            None => {
                return Err(ResolveError::ItemFailed {
                    module: alias_module.to_vec(),
                    item: alias_name.clone(),
                });
            }
        }
        let written = r#type.clone();
        let run = self.with_analyzer(alias_module, &substitution, owner, |analyzer| {
            analyzer.resolve_under_substitution(owner.id, owner.span, &written, &[])
        });
        match (run.failed, run.result) {
            (false, Some(resolved)) => Ok(ResolvedItem::Type(resolved)),
            _ => Err(ResolveError::ItemFailed {
                module: alias_module.to_vec(),
                item: alias_name.clone(),
            }),
        }
    }
}

impl ModuleResolver for Driver {
    fn macro_origin_module(&self, origin: omega_parser::prelude::Origin) -> Option<Vec<Ident>> {
        self.modules.macro_origin_module(origin)
    }

    fn macro_origin_visibility(&self, origin: omega_parser::prelude::Origin) -> Option<Visibility> {
        self.modules.macro_origin_visibility(origin)
    }

    fn resolve_explicit_anchor(
        &self,
        origin_module: &[Ident],
        path: &omega_parser::prelude::Path,
    ) -> Option<Result<Vec<Ident>, ResolveError>> {
        Driver::resolve_explicit_anchor(origin_module, path)
    }

    fn resolve_module_path(
        &mut self,
        accessor: &[Ident],
        absolute_path: &[Ident],
    ) -> Result<Option<Vec<Ident>>, ResolveError> {
        let Some((root, rest)) = absolute_path.split_first() else {
            return Ok(None);
        };
        let mut resolved = vec![root.clone()];
        match self.roots.locate(&resolved) {
            Ok(_) => {}
            Err(ResolveError::UnknownModule(_)) => return Ok(None),
            Err(error) => return Err(error),
        }

        for segment in rest {
            let direct: Vec<Ident> = resolved
                .iter()
                .cloned()
                .chain(std::iter::once(segment.clone()))
                .collect();
            match self.roots.locate(&direct) {
                Ok(_) => {
                    resolved = direct;
                    continue;
                }
                Err(ResolveError::UnknownModule(_)) => {}
                Err(error) => return Err(error),
            }
            // A speculative probe: this function only answers "is the whole
            // path a module," so a name that exists but is not a visible
            // module alias here -- hidden, or an alias of something else
            // entirely -- simply means the path is not a module, exactly
            // like the unknown-module case just above. Surfacing that as a
            // hard error would incorrectly fail unrelated resolution that
            // only reaches here to test the module reading (e.g. resolving
            // an import target that turns out to name an item, not a
            // nested module).
            match self.visible_alias(accessor, &resolved, segment, false) {
                Ok(Some(ResolvedAlias::Module(target))) => resolved = target,
                Ok(Some(_)) | Ok(None) | Err(ResolveError::NotVisible { .. }) => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Some(resolved))
    }

    fn declared_item_visibility(&mut self, absolute_path: &[Ident]) -> Option<Visibility> {
        let (name, module) = absolute_path.split_last()?;
        self.alias_visibility(module, name)
            .or_else(|| self.declared_visibility(module, name))
    }

    /// A declared alias binds a name in the same position an import alias
    /// does, so path heads reach both through this one query. Only the two
    /// path-shaped alias targets appear here; a structural type target has no
    /// absolute path and is expanded by `aliases::expand_type_alias` instead.
    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        match self.declared_alias(module_path, alias)? {
            Some(ResolvedAlias::Module(target)) => return Ok(Some(ImportTarget::Module(target))),
            // Keep the alias binding itself as the lazy path. Collapsing an
            // overload alias to its target group would throw away the frozen
            // candidate subset stored on the alias; keeping the binding also
            // makes ordinary item aliases follow the same gate-preserving
            // path as directly imported aliases.
            Some(ResolvedAlias::Item(_)) | Some(ResolvedAlias::Overloads { .. }) => {
                let absolute = module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(alias.clone()))
                    .collect();
                return Ok(Some(ImportTarget::ItemPath(ItemAccess::authorized(
                    absolute,
                ))));
            }
            Some(ResolvedAlias::Type { .. }) => return Ok(None),
            None => {}
        }
        self.resolve_import_alias_entry(module_path, alias)
    }

    fn resolve_visible_alias(
        &mut self,
        accessor: &[Ident],
        alias_module: &[Ident],
        name: &Ident,
        bypass_visibility: bool,
    ) -> Result<Option<ResolvedAlias>, ResolveError> {
        self.visible_alias(accessor, alias_module, name, bypass_visibility)
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

    fn resolve_item(
        &mut self,
        accessor_module_path: &[Ident],
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        options: ResolveItemOptions,
    ) -> Result<ResolvedItem, ResolveError> {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        if let Some(target) = self.declared_alias(module_path, item_name)? {
            return self.resolve_through_alias(
                accessor_module_path,
                module_path,
                item_name,
                target,
                type_args,
                options,
            );
        }
        self.ensure_item(
            accessor_module_path,
            module_path,
            item_name,
            type_args,
            options,
        )
    }

    fn is_item_visible(&mut self, accessor_module_path: &[Ident], absolute_path: &[Ident]) -> bool {
        let Some((_, module_path)) = absolute_path.split_last() else {
            return false;
        };
        let module_path = module_path.to_vec();
        self.declared_item_visibility(absolute_path)
            .is_some_and(|visibility| {
                Self::visibility_allows(visibility, &module_path, accessor_module_path)
            })
    }

    fn fresh_synthetic_id(&mut self) -> HirId {
        self.items.fresh_synthetic_id()
    }

    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError> {
        let absolute_path = self.canonical_query_path(absolute_path);
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let HirItem::FunctionDefinition(f) = &self.modules.parsed(module_path).hir.items[index]
        else {
            return Ok(None);
        };
        let f = f.clone();
        let f = self.normalized_function(module_path, &f)?;
        if f.generics.is_empty() {
            return Ok(None);
        }
        Ok(Some(GenericSignature {
            generics: f.generics.iter().map(|g| g.ident.clone()).collect(),
            defaults: f.generics.iter().map(|g| g.default.clone()).collect(),
            params: f.params.iter().map(|p| p.r#type.clone()).collect(),
            return_type: f.return_type.clone(),
        }))
    }

    fn generic_literal_signature(
        &mut self,
        absolute_path: &[Ident],
        variant: Option<&Ident>,
    ) -> Result<Option<GenericLiteralSignature>, ResolveError> {
        let absolute_path = self.canonical_query_path(absolute_path);
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
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

    fn generic_owner_function_signature(
        &mut self,
        owner_absolute: &[Ident],
        function_name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Option<GenericOwnerFunctionSignature>, ResolveError> {
        let owner_absolute = self.canonical_query_path(owner_absolute);
        let Some((name, module_path)) = owner_absolute.split_last() else {
            return Err(ResolveError::UnknownModule(owner_absolute.to_vec()));
        };
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
        // Exactly one candidate only -- 2+ overloads under this name is
        // `resolve_type_qualified_overload_call`'s own concern (once the
        // owner is concrete); this must not silently pick a first match.
        let mut matches = functions.iter().filter(|f| {
            &f.name == function_name && FunctionNamespace::of_declaration(f.self_mode) == namespace
        });
        let f = matches.next();
        if matches.next().is_some() {
            return Ok(None);
        }
        let Some(f) = f else {
            return Ok(None);
        };
        let generics: Vec<Ident> = owner_generics.iter().map(|g| g.ident.clone()).collect();
        Ok(Some(GenericOwnerFunctionSignature {
            owner_generics: generics.clone(),
            owner_defaults: owner_generics.iter().map(|g| g.default.clone()).collect(),
            function_generics: f.generics.iter().map(|g| g.ident.clone()).collect(),
            // A member's synthetic receiver is written in terms of `Self`,
            // which inference cannot solve; the owner applied to its own
            // generic parameters is the same type and is solvable.
            params: f
                .params
                .iter()
                .map(|p| rewrite_self(&p.r#type, name, &generics))
                .collect(),
            return_type: rewrite_self(&f.return_type, name, &generics),
        }))
    }

    fn spec_declaration(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError> {
        let absolute_path = self.canonical_query_path(absolute_path);
        self.resolve_spec_declaration(&absolute_path)
    }

    fn resolve_overload_set(
        &mut self,
        accessor: &[Ident],
        access: &ItemAccess,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError> {
        let Some((name, module)) = access.absolute.split_last() else {
            return Ok(None);
        };
        let (name, module) = (name.clone(), module.to_vec());
        // A path whose prefix is not a module simply is not an overload set
        // here; that is a question for whichever reading of the path does
        // apply, not a failure of this one.
        if self.ensure_module_indexed(&module).is_err() {
            return Ok(None);
        }

        if let Some(target) =
            self.visible_alias(accessor, &module, &name, access.bypass_visibility)?
        {
            let ResolvedAlias::Overloads {
                absolute,
                candidates,
            } = target
            else {
                return Ok(None);
            };
            let (target_name, target_module) = absolute
                .split_last()
                .expect("an overload alias target is never empty");
            let Some(raw) = self.raw_overload_signatures(target_module, target_name)? else {
                return Ok(None);
            };
            return Ok(Some(ResolvedOverloadSet {
                candidates: raw
                    .into_iter()
                    .filter(|candidate| candidates.contains(&candidate.decl_id))
                    .collect(),
                absolute,
            }));
        }

        let Some(raw) = self.raw_overload_signatures(&module, &name)? else {
            return Ok(None);
        };
        let candidates = if access.bypass_visibility {
            raw
        } else {
            raw.into_iter()
                .filter(|candidate| {
                    Self::visibility_allows(candidate.visibility, &module, accessor)
                })
                .collect()
        };
        Ok(Some(ResolvedOverloadSet {
            absolute: access.absolute.clone(),
            candidates,
        }))
    }

    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident> {
        if self.ensure_module_indexed(module_path).is_err() {
            return None;
        }
        // An alias's namespace is only known once its target resolves, so
        // alias names are offered as candidates in either namespace.
        let declared_aliases = self.alias_names(module_path);
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
                | HirItem::Declaration { .. }
                | HirItem::DeclarationWithInit { .. }
                | HirItem::Walrus { .. }
                | HirItem::ForeignBinding(_)
                | HirItem::ForeignFunction(_) => namespace == ItemNamespace::Value,
                HirItem::Glue(_)
                | HirItem::Conform(_)
                | HirItem::Primitive(_)
                | HirItem::Import(_)
                | HirItem::Alias(_) => false,
            })
            .map(|(name, _)| name)
            .chain(declared_aliases.iter());
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
            Driver::conformance_for(self, target, spec, spec_args).map(|entry| {
                ResolvedConformance {
                    target: entry.target,
                    spec: entry.spec,
                    spec_args: entry.spec_args,
                    methods: entry.methods,
                }
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

    fn conformances_for_specs(
        &mut self,
        target: &ResolvedType,
        spec_ids: &[HirId],
    ) -> Result<Vec<ResolvedConformance>, ResolveError> {
        for id in spec_ids {
            self.solve(target, Some(id));
        }
        Ok(self
            .conformances
            .entries
            .iter()
            .filter(|entry| {
                entry.target == target.lookup_key() && spec_ids.contains(&entry.spec.borrow().id)
            })
            .map(|entry| ResolvedConformance {
                target: entry.target.clone(),
                spec: entry.spec.clone(),
                spec_args: entry.spec_args.clone(),
                methods: entry.methods.clone(),
            })
            .collect())
    }

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
            CheckedItem::FunctionDefinition(f) => Some(f),
            CheckedItem::Struct(s) => s.functions.into_iter().find(|f| f.id == decl_id),
            CheckedItem::Enum(e) => e.functions.into_iter().find(|f| f.id == decl_id),
            CheckedItem::Union(u) => u.functions.into_iter().find(|f| f.id == decl_id),
            CheckedItem::Declaration(_)
            | CheckedItem::ForeignBinding(_)
            | CheckedItem::ForeignFunction(_) => None,
        })
    }

    fn resolve_comp_value(&mut self, decl_id: HirId) -> Option<ConstValue> {
        self.items.comp_values.get(&decl_id).cloned()
    }
}

fn rewrite_self(ty: &Type, owner: &Ident, owner_generics: &[Ident]) -> Type {
    match ty {
        Type::Named(path) if path.is_unqualified() && &path.head == &Ident("Self".to_string()) => {
            Type::Generic(
                Path::from(owner.clone()),
                owner_generics
                    .iter()
                    .map(|generic| Type::Named(Path::from(generic.clone())))
                    .collect(),
            )
        }
        Type::Pointer(inner, mutable) => Type::Pointer(
            Box::new(rewrite_self(inner, owner, owner_generics)),
            *mutable,
        ),
        Type::InferredArray(inner) => {
            Type::InferredArray(Box::new(rewrite_self(inner, owner, owner_generics)))
        }
        Type::UnknownSizeArray(inner) => {
            Type::UnknownSizeArray(Box::new(rewrite_self(inner, owner, owner_generics)))
        }
        Type::SizedArray(inner, n) => Type::SizedArray(
            Box::new(rewrite_self(inner, owner, owner_generics)),
            n.clone(),
        ),
        Type::SpecStatic(members) => Type::SpecStatic(
            members
                .iter()
                .map(|m| rewrite_self(m, owner, owner_generics))
                .collect(),
        ),
        Type::Generic(path, args) => Type::Generic(
            path.clone(),
            args.iter()
                .map(|arg| rewrite_self(arg, owner, owner_generics))
                .collect(),
        ),
        Type::Function(f) => Type::Function(FunctionType {
            params: f
                .params
                .iter()
                .map(|p| omega_parser::prelude::FunctionTypeParam {
                    r#type: rewrite_self(&p.r#type, owner, owner_generics),
                    ..p.clone()
                })
                .collect(),
            return_type: Box::new(rewrite_self(&f.return_type, owner, owner_generics)),
            is_variadic: f.is_variadic,
            self_mode: f.self_mode,
            convention: f.convention.clone(),
        }),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> Ident {
        Ident(name.to_string())
    }

    #[test]
    fn import_cycle_path_preserves_resolution_order() {
        let first = (vec![ident("a")], ident("x"));
        let second = (vec![ident("b")], ident("y"));
        let mut state = ImportState::default();
        state.begin_resolution(&first);
        state.begin_resolution(&second);

        assert_eq!(
            state.cycle_path(&first),
            vec![
                vec![ident("a"), ident("x")],
                vec![ident("b"), ident("y")],
                vec![ident("a"), ident("x")],
            ]
        );
    }
}
