use crate::{Driver, ModulePath};
use omega_analyzer::analysis::item_visibility;
use omega_analyzer::checked::{CheckedFunctionDef, CheckedItem};
use omega_analyzer::resolved_type::{
    ConstValue, ResolvedConformance, ResolvedMethod, ResolvedSpecType, ResolvedType,
};
use omega_analyzer::resolver::{
    GenericLiteralSignature, GenericSignature, GenericStaticFunctionSignature, ImportTarget,
    ItemNamespace, ModuleResolver, OverloadCandidate, OverloadCandidates, ResolveError,
    ResolveItemOptions, ResolvedItem,
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
            // Real either way -- must surface here, not be masked by the
            // item-import fallback below.
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }

        let Some((item_name, module_path)) = segments.split_last() else {
            return Err(ResolveError::UnknownModule(segments.to_vec()));
        };

        if self.is_generic_template(module_path, item_name)? {
            return Ok(ImportTarget::GenericItem(segments.to_vec()));
        }

        let item = self.ensure_item(
            accessor,
            module_path,
            item_name,
            &[],
            ResolveItemOptions::INDIRECT.bypassing_visibility(reveal),
        )?;
        Ok(ImportTarget::Item(segments.to_vec(), item))
    }

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
        self.imports.mark_used(module_path, alias);
        Ok(Some(entry))
    }
}

impl ModuleResolver for Driver {
    fn macro_origin_module(&self, origin: omega_parser::prelude::Origin) -> Option<Vec<Ident>> {
        self.modules.macro_origin_module(origin)
    }

    fn macro_origin_visibility(&self, origin: omega_parser::prelude::Origin) -> Option<Visibility> {
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
        options: ResolveItemOptions,
    ) -> Result<ResolvedItem, ResolveError> {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        self.ensure_item(
            accessor_module_path,
            module_path,
            item_name,
            type_args,
            options,
        )
    }

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
                &owner_generics
                    .iter()
                    .map(|g| g.ident.clone())
                    .collect::<Vec<_>>(),
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

    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident> {
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
                | HirItem::ForeignBinding(_)
                | HirItem::ForeignFunction(_) => namespace == ItemNamespace::Value,
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

fn rewrite_self_return(ty: &Type, owner: &Ident, owner_generics: &[Ident]) -> Type {
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
            Box::new(rewrite_self_return(inner, owner, owner_generics)),
            *mutable,
        ),
        Type::InferredArray(inner) => {
            Type::InferredArray(Box::new(rewrite_self_return(inner, owner, owner_generics)))
        }
        Type::UnknownSizeArray(inner) => {
            Type::UnknownSizeArray(Box::new(rewrite_self_return(inner, owner, owner_generics)))
        }
        Type::SizedArray(inner, n) => Type::SizedArray(
            Box::new(rewrite_self_return(inner, owner, owner_generics)),
            n.clone(),
        ),
        Type::SpecStatic(members) => Type::SpecStatic(
            members
                .iter()
                .map(|m| rewrite_self_return(m, owner, owner_generics))
                .collect(),
        ),
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
