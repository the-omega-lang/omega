use crate::checked::Storage;
use crate::error::TypeResolutionError;
use crate::resolved_type::{CallingConvention, ResolvedFunctionType, ResolvedType};
use crate::resolver::{
    ImportTarget, ItemNamespace, ModuleResolver, ResolveError, ResolveItemOptions, ResolvedItem,
};
use crate::similarity::best_match;
use crate::target::Target;
use indexmap::IndexMap;
use omega_hir::HirId;
use omega_parser::prelude::*;

#[derive(Debug, Clone)]
pub struct VarBinding {
    pub decl_id: HirId,
    pub storage: Storage,
    pub r#type: ResolvedType,
    pub span: Span,
    pub narrowed: bool,
    pub mutable: bool,
    pub used: bool,
    pub written: bool,
}

#[derive(Debug, Clone)]
pub struct LexicalScope {
    declared_variables: IndexMap<(Ident, Origin), VarBinding>,
    defined_types: IndexMap<Ident, ResolvedType>,
}

impl LexicalScope {
    fn new() -> Self {
        Self {
            declared_variables: IndexMap::new(),
            defined_types: IndexMap::new(),
        }
    }

    pub fn declare(
        &mut self,
        ident: Ident,
        origin: Origin,
        binding: VarBinding,
    ) -> Result<(), (Ident, Span)> {
        if let Some(existing) = self.declared_variables.get(&(ident.clone(), origin)) {
            return Err((ident, existing.span));
        }
        self.declared_variables.insert((ident, origin), binding);
        Ok(())
    }

    pub fn bindings(&self) -> impl Iterator<Item = (&(Ident, Origin), &VarBinding)> {
        self.declared_variables.iter()
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    scopes: Vec<LexicalScope>,
    comp_values: std::collections::HashMap<HirId, crate::resolved_type::ConstValue>,
    target: Target,
}

impl Context {
    pub fn new(target: Target) -> Self {
        let mut global_scope = LexicalScope::new();
        global_scope.defined_types.extend([
            (Ident("void".into()), ResolvedType::Void),
            (Ident("never".into()), ResolvedType::Never),
            (Ident("bool".into()), ResolvedType::Bool),
            (Ident("char".into()), ResolvedType::Char),
            (Ident("i8".into()), ResolvedType::I8),
            (Ident("i16".into()), ResolvedType::I16),
            (Ident("i32".into()), ResolvedType::I32),
            (Ident("i64".into()), ResolvedType::I64),
            (Ident("isize".into()), ResolvedType::ISize),
            (Ident("u8".into()), ResolvedType::U8),
            (Ident("u16".into()), ResolvedType::U16),
            (Ident("u32".into()), ResolvedType::U32),
            (Ident("u64".into()), ResolvedType::U64),
            (Ident("usize".into()), ResolvedType::USize),
            (Ident("f32".into()), ResolvedType::F32),
            (Ident("f64".into()), ResolvedType::F64),
        ]);
        Self {
            scopes: vec![global_scope],
            comp_values: std::collections::HashMap::new(),
            target,
        }
    }

    /// Resolves a source-level convention name against the target, or `None`
    /// for the implicit Omega convention. Availability (e.g. `sysv64` only on
    /// x86-64) is enforced here so every caller gets the same diagnostic.
    pub fn resolve_convention(
        &self,
        convention: Option<&Ident>,
    ) -> Result<CallingConvention, TypeResolutionError> {
        let Some(name) = convention else {
            return Ok(CallingConvention::Omega);
        };
        let resolved = match name.as_ref() {
            "c" => CallingConvention::C,
            "sysv64" => CallingConvention::SysV64,
            _ => {
                return Err(TypeResolutionError::UnknownCallingConvention {
                    name: name.clone(),
                });
            }
        };
        if !resolved.is_available_on(self.target) {
            return Err(TypeResolutionError::CallingConventionNotAvailable {
                name: name.clone(),
                convention: resolved,
                target: self.target,
            });
        }
        Ok(resolved)
    }

    pub fn set_comp_value(&mut self, decl_id: HirId, value: crate::resolved_type::ConstValue) {
        self.comp_values.insert(decl_id, value);
    }

    pub fn comp_value(&self, decl_id: HirId) -> Option<&crate::resolved_type::ConstValue> {
        self.comp_values.get(&decl_id)
    }

    pub fn find_variable(&self, ident: &Ident, origin: Origin) -> Option<&VarBinding> {
        let key = (ident.clone(), origin);
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.declared_variables.get(&key))
            .or_else(|| {
                if ident.as_ref() != "self" || origin == Origin::default() {
                    return None;
                }
                let root_key = (ident.clone(), Origin::default());
                self.scopes
                    .iter()
                    .rev()
                    .find_map(|scope| scope.declared_variables.get(&root_key))
            })
    }

    pub fn widen_variable(&mut self, ident: &Ident, origin: Origin) {
        let key = (ident.clone(), origin);
        if let Some(binding) = self.variable_mut(&key) {
            binding.r#type = binding.r#type.widened();
            return;
        }
        if ident.as_ref() == "self" && origin != Origin::default() {
            let root_key = (ident.clone(), Origin::default());
            if let Some(binding) = self.variable_mut(&root_key) {
                binding.r#type = binding.r#type.widened();
            }
        }
    }

    pub fn mark_used(&mut self, decl_id: HirId) {
        if let Some(binding) = self.binding_mut(decl_id) {
            binding.used = true;
        }
    }

    pub fn mark_written(&mut self, decl_id: HirId) {
        if let Some(binding) = self.binding_mut(decl_id) {
            binding.written = true;
        }
    }

    fn variable_mut(&mut self, key: &(Ident, Origin)) -> Option<&mut VarBinding> {
        self.scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.declared_variables.get_mut(key))
    }

    fn binding_mut(&mut self, decl_id: HirId) -> Option<&mut VarBinding> {
        self.scopes.iter_mut().rev().find_map(|scope| {
            scope
                .declared_variables
                .values_mut()
                .find(|binding| binding.decl_id == decl_id)
        })
    }

    pub fn find_defined_type(&self, name: &Ident) -> Option<&ResolvedType> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.defined_types.get(name))
    }

    pub fn similar_variable_name(&self, target: &Ident) -> Option<Ident> {
        best_match(
            target,
            self.scopes
                .iter()
                .flat_map(|scope| scope.declared_variables.keys().map(|(ident, _)| ident)),
        )
    }

    pub fn similar_type_name(&self, target: &Ident) -> Option<Ident> {
        best_match(
            target,
            self.scopes
                .iter()
                .flat_map(|scope| scope.defined_types.keys()),
        )
    }

    pub fn resolve_function_type(
        &self,
        fntype: FunctionType,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<ResolvedFunctionType, TypeResolutionError> {
        let params = fntype
            .params
            .into_iter()
            .map(|param| {
                self.resolve_type(
                    param.r#type,
                    resolver,
                    module_path,
                    options.through_indirection(),
                )
                .map(|resolved| (param.ident, resolved))
            })
            .collect::<Result<Vec<(Ident, ResolvedType)>, TypeResolutionError>>()?;
        let calling_convention =
            self.resolve_convention(fntype.convention.as_ref().map(|c| &c.name))?;
        if fntype.is_variadic && !calling_convention.supports_variadic() {
            return Err(TypeResolutionError::VariadicNotSupportedByConvention {
                convention: calling_convention,
            });
        }
        Ok(ResolvedFunctionType {
            params,
            return_type: Box::new(self.resolve_type(
                *fntype.return_type,
                resolver,
                module_path,
                options.through_indirection(),
            )?),
            is_variadic: fntype.is_variadic,
            self_mode: fntype.self_mode,
            calling_convention,
        })
    }

    pub(crate) fn resolve_absolute_item_path(
        &self,
        resolver: &mut dyn ModuleResolver,
        path: &Path,
        module_path: &[Ident],
    ) -> Result<Vec<Ident>, TypeResolutionError> {
        let resolution_module = resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| module_path.to_vec());
        if path.is_unqualified() {
            // `ImportTarget::Item`'s eagerly-resolved snapshot is ignored
            // here -- this function's only job is the absolute path; every
            // caller re-resolves through `resolver` with its own real
            // `indirect`/args, never trusting a cached snapshot.
            match resolver
                .resolve_import_alias(&resolution_module, &path.head)
                .map_err(TypeResolutionError::ModuleResolution)?
            {
                Some(ImportTarget::GenericItem(absolute)) => return Ok(absolute),
                Some(ImportTarget::Item(absolute, _)) => return Ok(absolute),
                _ => {}
            }
            Ok(resolution_module
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect())
        } else {
            match resolver
                .resolve_import_alias(&resolution_module, &path.head)
                .map_err(TypeResolutionError::ModuleResolution)?
            {
                Some(ImportTarget::Module(target)) => Ok(target
                    .into_iter()
                    .chain(path.tail.iter().cloned())
                    .collect()),
                _ => Err(TypeResolutionError::ModuleNotImported {
                    name: path.head.clone(),
                    similar: best_match(
                        &path.head,
                        resolver.import_alias_names(&resolution_module).iter(),
                    ),
                }),
            }
        }
    }

    pub fn resolve_type(
        &self,
        typ: Type,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<ResolvedType, TypeResolutionError> {
        match typ {
            Type::Named(path) => self.resolve_named_type(path, resolver, module_path, options),
            Type::Generic(path, args) => {
                self.resolve_generic_type(path, args, resolver, module_path, options)
            }
            Type::Pointer(pointee, mutable) => {
                self.resolve_pointer_type(*pointee, mutable, resolver, module_path, options)
            }
            Type::SpecObject(pointee, mutable) => {
                self.resolve_spec_object_type(*pointee, mutable, resolver, module_path, options)
            }
            Type::SpecStatic(pointee) => {
                let name = match pointee.as_ref() {
                    Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
                    _ => Ident("<spec>".to_string()),
                };
                Err(TypeResolutionError::SpecStaticNotAllowedHere(name))
            }
            Type::Function(fntyp) => Ok(ResolvedType::Function(self.resolve_function_type(
                fntyp,
                resolver,
                module_path,
                options,
            )?)),
            Type::InferredArray(_) => Err(TypeResolutionError::BareUnsizedArray),
            Type::UnknownSizeArray(_) => Err(TypeResolutionError::BareUnknownSizeArray),
            Type::SizedArray(item, size) => {
                let size = size
                    .parse::<u32>()
                    .map_err(|_| TypeResolutionError::InvalidArraySize(size.clone()))?;
                let item = self.resolve_type(*item, resolver, module_path, options)?;
                Ok(ResolvedType::SizedArray(Box::new(item), size))
            }
        }
    }

    fn resolve_named_type(
        &self,
        path: Path,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolution_module = resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| module_path.to_vec());
        let resolved = {
            if let Some(resolved) =
                self.try_resolve_enum_variant_type(&path, resolver, module_path, options)?
            {
                resolved
            } else if path.is_unqualified() {
                if let Some(local) = self.find_defined_type(&path.head) {
                    local.to_owned()
                } else {
                    // An import alias, lazily resolved: find the
                    // absolute path the alias names, then resolve *that*
                    // through `resolve_item` with this reference's own
                    // `indirect` -- deliberately never trusting
                    // `ImportTarget::Item`'s eagerly-resolved snapshot,
                    // which was always produced with `indirect = true`
                    // (see findings for the cycle-detection bug this
                    // used to cause). Re-running `resolve_item` costs
                    // nothing extra once the item is already resolved.
                    let alias = resolver
                        .resolve_import_alias(&resolution_module, &path.head)
                        .map_err(TypeResolutionError::ModuleResolution)?;
                    if let Some(ImportTarget::Item(_, ResolvedItem::Value { .. })) = alias {
                        return Err(TypeResolutionError::NotAType(vec![path.head.clone()]));
                    }
                    let absolute = match alias {
                        Some(ImportTarget::Item(absolute, _))
                        | Some(ImportTarget::GenericItem(absolute))
                        | Some(ImportTarget::Module(absolute)) => absolute,
                        None => resolution_module
                            .iter()
                            .cloned()
                            .chain(std::iter::once(path.head.clone()))
                            .collect(),
                    };
                    match resolver.resolve_item(&resolution_module, &absolute, &[], options) {
                        Ok(ResolvedItem::Type(t)) => t,
                        Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                            return Err(TypeResolutionError::NotAType(absolute));
                        }
                        // A bare type name that doesn't exist gets one
                        // more try against every exposed name in `core`'s
                        // ambient tree before giving up with a typo
                        // suggestion -- mirrors `resolve_generic_type`'s
                        // identical fallback, needed here too since a bare
                        // named type never reaches that function.
                        Err(ResolveError::UnknownItem { .. }) => {
                            match resolver.ambient_core_candidates(&resolution_module, &path.head) {
                                Ok(Some(ambient_absolute)) => {
                                    match resolver.resolve_item(
                                        &resolution_module,
                                        &ambient_absolute,
                                        &[],
                                        options,
                                    ) {
                                        Ok(ResolvedItem::Type(t)) => t,
                                        Ok(ResolvedItem::Value { .. })
                                        | Ok(ResolvedItem::Gap(_)) => {
                                            return Err(TypeResolutionError::NotAType(
                                                ambient_absolute,
                                            ));
                                        }
                                        Err(e) => {
                                            return Err(TypeResolutionError::ModuleResolution(e));
                                        }
                                    }
                                }
                                Ok(None) => {
                                    let similar = self
                                        .similar_type_name(&path.head)
                                        .or_else(|| {
                                            best_match(
                                                &path.head,
                                                resolver
                                                    .import_alias_names(&resolution_module)
                                                    .iter(),
                                            )
                                        })
                                        .or_else(|| {
                                            resolver.similar_item_name(
                                                &resolution_module,
                                                &path.head,
                                                ItemNamespace::Type,
                                            )
                                        });
                                    return Err(TypeResolutionError::UnrecognizedNamedType {
                                        name: path.head.clone(),
                                        similar,
                                    });
                                }
                                Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                            }
                        }
                        Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                    }
                }
            } else {
                let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
                match resolver
                    .resolve_item(&resolution_module, &absolute, &[], options)
                    .map_err(TypeResolutionError::ModuleResolution)?
                {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } | ResolvedItem::Gap(_) => {
                        return Err(TypeResolutionError::NotAType(absolute));
                    }
                }
            }
        };
        Ok(resolved)
    }

    fn resolve_generic_type(
        &self,
        path: Path,
        args: Vec<Type>,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolution_module = resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| module_path.to_vec());
        let resolved = {
            let resolved_args = args
                .into_iter()
                .map(|arg| {
                    self.resolve_type(arg, resolver, module_path, options.through_indirection())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
            let result =
                resolver.resolve_item(&resolution_module, &absolute, &resolved_args, options);
            let result = match (&result, path.is_unqualified()) {
                (Err(ResolveError::UnknownItem { .. }), true) => {
                    match resolver.ambient_core_candidates(&resolution_module, &path.head) {
                        Ok(Some(ambient_absolute)) => resolver.resolve_item(
                            &resolution_module,
                            &ambient_absolute,
                            &resolved_args,
                            options,
                        ),
                        Ok(None) => result,
                        Err(e) => Err(e),
                    }
                }
                _ => result,
            };
            match result.map_err(TypeResolutionError::ModuleResolution)? {
                ResolvedItem::Type(t) => t,
                ResolvedItem::Value { .. } | ResolvedItem::Gap(_) => {
                    return Err(TypeResolutionError::NotAType(absolute));
                }
            }
        };
        Ok(resolved)
    }

    fn resolve_pointer_type(
        &self,
        pointee_type: Type,
        mutable: bool,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<ResolvedType, TypeResolutionError> {
        match pointee_type {
            Type::InferredArray(item) => {
                let item =
                    self.resolve_type(*item, resolver, module_path, options.through_indirection())?;
                Ok(ResolvedType::Slice {
                    item: Box::new(item),
                    mutable,
                })
            }
            Type::UnknownSizeArray(item) => {
                let item =
                    self.resolve_type(*item, resolver, module_path, options.through_indirection())?;
                Ok(ResolvedType::Array(Box::new(item), mutable))
            }
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "str" => {
                Ok(ResolvedType::Str { mutable })
            }
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "Self" => {
                match self.resolve_named_type(
                    path,
                    resolver,
                    module_path,
                    options.through_indirection(),
                )? {
                    ResolvedType::Str { .. } => Ok(ResolvedType::Str { mutable }),
                    ResolvedType::Array(item, _) => Ok(ResolvedType::Slice { item, mutable }),
                    ResolvedType::Slice { item, .. } => Ok(ResolvedType::Slice { item, mutable }),
                    resolved => Ok(ResolvedType::Pointer {
                        pointee: Box::new(resolved),
                        mutable,
                    }),
                }
            }
            other => {
                let resolved =
                    self.resolve_type(other, resolver, module_path, options.through_indirection())?;
                Ok(ResolvedType::Pointer {
                    pointee: Box::new(resolved),
                    mutable,
                })
            }
        }
    }

    fn resolve_spec_object_type(
        &self,
        pointee: Type,
        mutable: bool,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let pointee = Box::new(pointee);
        let resolved = {
            let type_args = match pointee.as_ref() {
                Type::Generic(_, args) => args.clone(),
                _ => vec![],
            };
            let resolved_args = type_args
                .into_iter()
                .map(|a| self.resolve_type(a, resolver, module_path, options.through_indirection()))
                .collect::<Result<Vec<_>, _>>()?;
            let pointee_name = match pointee.as_ref() {
                Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
                _ => Ident("<spec>".to_string()),
            };
            match self.resolve_type(
                *pointee,
                resolver,
                module_path,
                options.through_indirection(),
            )? {
                ResolvedType::Spec(spec) => {
                    if !spec.borrow().is_object_safe {
                        return Err(TypeResolutionError::SpecNotObjectSafe(pointee_name));
                    }
                    ResolvedType::SpecObject {
                        spec,
                        type_args: resolved_args,
                        mutable,
                    }
                }
                _ => return Err(TypeResolutionError::NotASpec(pointee_name)),
            }
        };
        Ok(resolved)
    }

    fn try_resolve_enum_variant_type(
        &self,
        path: &Path,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        options: ResolveItemOptions,
    ) -> Result<Option<ResolvedType>, TypeResolutionError> {
        let Some((variant_name, prefix_tail)) = path.tail.split_last() else {
            return Ok(None);
        };
        let prefix = Type::Named(Path {
            head: path.head.clone(),
            tail: prefix_tail.to_vec(),
            origin: path.origin,
        });
        let Ok(ResolvedType::Enum {
            cell,
            variant: None,
        }) = self.resolve_type(prefix, resolver, module_path, options)
        else {
            return Ok(None);
        };
        let found = cell.borrow().variant(variant_name).map(|(idx, _)| idx);
        match found {
            Some(idx) => Ok(Some(ResolvedType::Enum {
                cell: cell.clone(),
                variant: Some(idx),
            })),
            None => {
                let similar =
                    best_match(variant_name, cell.borrow().variants.iter().map(|v| &v.name));
                Err(TypeResolutionError::NoSuchVariantForType {
                    r#enum: cell.borrow().name.clone(),
                    name: variant_name.clone(),
                    similar,
                })
            }
        }
    }

    pub fn declare(
        &mut self,
        ident: Ident,
        origin: Origin,
        binding: VarBinding,
    ) -> Result<(), (Ident, Span)> {
        self.scopes
            .last_mut()
            .expect("context always has a root scope")
            .declare(ident, origin, binding)
    }

    pub fn current_scope_has_type(&self, name: &Ident) -> bool {
        self.scopes
            .last()
            .expect("context always has a root scope")
            .defined_types
            .contains_key(name)
    }

    pub fn define_type(&mut self, name: Ident, r#type: ResolvedType) {
        self.scopes
            .last_mut()
            .expect("context always has a root scope")
            .defined_types
            .insert(name, r#type);
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(LexicalScope::new());
    }

    pub fn leave_scope(&mut self) -> LexicalScope {
        assert!(
            self.scopes.len() > 1,
            "attempted to leave the analyzer's root scope"
        );
        self.scopes.pop().expect("scope count checked above")
    }
}

#[cfg(test)]
mod tests;
