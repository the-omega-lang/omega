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
use omega_parser::prelude::{FunctionType, Ident, Path, Type, Visibility};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// One import alias's resolution state -- the same white/gray/black cycle
/// guard items get, at `(module, alias)` granularity rather than whole-module:
/// a whole-module guard would deadlock two modules whose unrelated items
/// happen to cross-import each other. Per-alias, only a name that genuinely,
/// directly needs itself reports a cycle.
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
            // Real either way -- must surface here, not be masked by the
            // item-import fallback below.
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }

        let Some((item_name, module_path)) = segments.split_last() else {
            return Err(ResolveError::UnknownModule(segments.to_vec()));
        };

        // A generic item import supplies no type arguments (those only
        // appear at a use site), so eagerly instantiating here would always
        // fail; defer, carrying just the absolute path.
        if self.is_generic_template(module_path, item_name)? {
            return Ok(ImportTarget::GenericItem(segments.to_vec()));
        }

        // Always resolved indirect: the absolute path travels along with the
        // snapshot so a consumer whose own `indirect` differs can re-resolve.
        let item = self.ensure_item(accessor, module_path, item_name, &[], true, reveal)?;
        Ok(ImportTarget::Item(segments.to_vec(), item))
    }

    /// One import alias's own structural facts, or `None` when the module
    /// binds no such alias.
    pub(crate) fn import_entry(
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
            // implicitly available as a qualified-path prefix (see
            // docs/10-modules-and-linkage.md's "core is a prelude"), except
            // from within `core`'s own tree.
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
            // recorded elsewhere, so just skip it here.
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
        // re-derives and reports it identically.
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
            return_type: f.return_type.clone(),
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
        // path, which re-derives and reports them identically.
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
        // them identically.
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
        // owner is concrete); this must not silently pick a first match.
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
            return_type: rewrite_self_return(
                &f.return_type,
                name,
                &owner_generics.iter().map(|g| g.ident.clone()).collect::<Vec<_>>(),
            ),
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
        // segment" split) isn't a real module, which is exactly what a
        // `Module::Type::function` static-call path looks like from here.
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
                | HirItem::Declaration { .. }
                | HirItem::DeclarationWithInit { .. }
                | HirItem::Walrus { .. }
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

    fn conformances_for_specs(
        &mut self,
        target: &ResolvedType,
        spec_ids: &[HirId],
    ) -> Result<Vec<ResolvedConformance>, ResolveError> {
        // Goal-directed: `solve` per requested spec instantiates only the
        // templates that can produce it. Entries are then filtered down to
        // exactly the requested specs.
        for id in spec_ids {
            self.solve(target, Some(id));
        }
        Ok(self
            .conformances
            .entries
            .iter()
            .filter(|entry| {
                entry.target == target.lookup_key()
                    && spec_ids.contains(&entry.spec.borrow().id)
            })
            .map(|entry| ResolvedConformance {
                target: entry.target.clone(),
                spec: entry.spec.clone(),
                spec_args: entry.spec_args.clone(),
                methods: entry.methods.clone(),
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

/// Rewrites a `Self` leaf in a static function's declared return type to
/// the owner's own generic spelling (`Self` -> `Box<T>`, written
/// `Type::Generic` of the owner name path with each owner generic as an
/// unqualified `Named` leaf), recursing through the same shapes
/// `unify_generic_type` walks -- pointers, sized/inferred/unknown-size
/// arrays, spec objects, function types, generic applications. Needed so
/// `=> Self` unifies against an expected `Box<i32>` exactly the same way a
/// written `=> Box<T>` does: `unify_generic_type` only binds a `Named`
/// leaf whose name is one of the generics, and `Self` is not in that list
/// -- it is the whole *application*, not a parameter.
fn rewrite_self_return(ty: &Type, owner: &Ident, owner_generics: &[Ident]) -> Type {
    match ty {
        Type::Named(path)
            if path.is_unqualified() && &path.head == &Ident("Self".to_string()) =>
        {
            Type::Generic(
                Path::from(owner.clone()),
                owner_generics
                    .iter()
                    .map(|generic| Type::Named(Path::from(generic.clone())))
                    .collect(),
            )
        }
        Type::Pointer(inner, mutable) => {
            Type::Pointer(Box::new(rewrite_self_return(inner, owner, owner_generics)), *mutable)
        }
        Type::InferredArray(inner) => {
            Type::InferredArray(Box::new(rewrite_self_return(inner, owner, owner_generics)))
        }
        Type::UnknownSizeArray(inner) => {
            Type::UnknownSizeArray(Box::new(rewrite_self_return(inner, owner, owner_generics)))
        }
        Type::SizedArray(inner, n) => {
            Type::SizedArray(Box::new(rewrite_self_return(inner, owner, owner_generics)), n.clone())
        }
        Type::SpecObject(inner, mutable) => {
            Type::SpecObject(Box::new(rewrite_self_return(inner, owner, owner_generics)), *mutable)
        }
        Type::SpecStatic(inner) => {
            Type::SpecStatic(Box::new(rewrite_self_return(inner, owner, owner_generics)))
        }
        Type::Generic(path, args) => Type::Generic(
            path.clone(),
            args.iter()
                .map(|arg| rewrite_self_return(arg, owner, owner_generics))
                .collect(),
        ),
        Type::Function(f) => Type::Function(FunctionType {
            params: f
                .params
                .iter()
                .map(|p| omega_parser::prelude::Param {
                    r#type: rewrite_self_return(&p.r#type, owner, owner_generics),
                    ..p.clone()
                })
                .collect(),
            return_type: Box::new(rewrite_self_return(&f.return_type, owner, owner_generics)),
            is_variadic: f.is_variadic,
            self_mode: f.self_mode,
        }),
        other => other.clone(),
    }
}
