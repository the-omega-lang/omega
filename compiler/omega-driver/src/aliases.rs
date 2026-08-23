//! Driver-owned `alias` query state.
//!
//! An alias resolves to the identity its target already has. It never gets an
//! `ItemKey`, a type cell, or a symbol, so this query deliberately sits beside
//! the item queries rather than inside them.

use crate::{Driver, ModulePath};
use omega_analyzer::resolver::{ImportTarget, ModuleResolver, ResolveError, ResolvedAlias};
use omega_hir::{AliasTarget, HirAlias, HirGenericParam, HirItem};
use omega_parser::prelude::{FunctionType, Ident, Origin, Param, Path, Type, Visibility};
use std::collections::HashMap;

type AliasKey = (ModulePath, Ident);

enum AliasQueryState {
    InProgress,
    Resolved(Option<ResolvedAlias>),
    Failed(ResolveError),
}

#[derive(Default)]
pub(crate) struct AliasState {
    resolved: HashMap<AliasKey, AliasQueryState>,
    resolution_stack: Vec<AliasKey>,
}

impl AliasState {
    fn begin(&mut self, key: &AliasKey) {
        self.resolved.insert(key.clone(), AliasQueryState::InProgress);
        self.resolution_stack.push(key.clone());
    }

    fn finish(&mut self, key: &AliasKey, result: &Result<Option<ResolvedAlias>, ResolveError>) {
        let active = self
            .resolution_stack
            .pop()
            .expect("finishing an alias resolution requires an active query");
        assert_eq!(&active, key, "query stack must unwind in LIFO order");
        let state = match result {
            Ok(target) => AliasQueryState::Resolved(target.clone()),
            Err(error) => AliasQueryState::Failed(error.clone()),
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
            .map(|(module, name)| {
                let mut path = module.clone();
                path.push(name.clone());
                path
            })
            .collect()
    }
}

/// The declaration kinds an alias may name, and the wording used to reject the
/// rest. `None` means the item is a legal target.
fn rejected_target_kind(item: &HirItem) -> Option<&'static str> {
    match item {
        HirItem::Struct(_)
        | HirItem::Enum(_)
        | HirItem::Union(_)
        | HirItem::Spec(_)
        | HirItem::FunctionDefinition(_)
        | HirItem::ForeignFunction(_)
        | HirItem::Alias(_) => None,
        // A foreign binding is a function only when its declared type says so;
        // anything else is storage, and storage is a value, not a name.
        HirItem::ForeignBinding(binding) => match binding.r#type {
            Type::Function(_) => None,
            _ => Some("the global"),
        },
        HirItem::Declaration { .. } | HirItem::DeclarationWithInit { .. } => Some("the global"),
        HirItem::Walrus { walrus, .. } if walrus.comp => Some("the compile-time value"),
        HirItem::Walrus { .. } => Some("the global"),
        HirItem::Gap(_) => Some("the gap"),
        HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) | HirItem::Import(_) => {
            unreachable!("unnamed items are never reached by name")
        }
    }
}

/// Retags every path in a written type with `origin`, so the type keeps
/// resolving in the alias declaration's module after it is substituted into a
/// use site elsewhere. This reuses the provenance mechanism macro expansion
/// already relies on for exactly the same reason.
fn retag_origins(ty: &Type, origin: Origin) -> Type {
    let recur = |t: &Type| retag_origins(t, origin);
    let retag_path = |path: &Path| Path {
        head: path.head.clone(),
        tail: path.tail.clone(),
        origin,
    };
    match ty {
        Type::Named(path) => Type::Named(retag_path(path)),
        Type::Generic(path, args) => {
            Type::Generic(retag_path(path), args.iter().map(recur).collect())
        }
        Type::Pointer(inner, mutable) => Type::Pointer(Box::new(recur(inner)), *mutable),
        Type::InferredArray(inner) => Type::InferredArray(Box::new(recur(inner))),
        Type::UnknownSizeArray(inner) => Type::UnknownSizeArray(Box::new(recur(inner))),
        Type::SizedArray(inner, size) => Type::SizedArray(Box::new(recur(inner)), size.clone()),
        Type::SpecStatic(members) => Type::SpecStatic(members.iter().map(recur).collect()),
        Type::Function(f) => Type::Function(FunctionType {
            params: f
                .params
                .iter()
                .map(|p| Param {
                    r#type: recur(&p.r#type),
                    ..p.clone()
                })
                .collect(),
            return_type: Box::new(recur(&f.return_type)),
            is_variadic: f.is_variadic,
            self_mode: f.self_mode,
            convention: f.convention.clone(),
        }),
    }
}

impl Driver {
    pub(crate) fn alias_index(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<usize>, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        Ok(self.modules.index(module_path).aliases.get(name).copied())
    }

    fn optional_item_index(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<usize>, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        Ok(self.modules.index(module_path).items.get(name).copied())
    }

    pub(crate) fn declared_alias(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<ResolvedAlias>, ResolveError> {
        let Some(index) = self.alias_index(module_path, name)? else {
            return Ok(None);
        };
        let key: AliasKey = (module_path.to_vec(), name.clone());
        match self.aliases.resolved.get(&key) {
            Some(AliasQueryState::Resolved(target)) => return Ok(target.clone()),
            Some(AliasQueryState::Failed(error)) => return Err(error.clone()),
            Some(AliasQueryState::InProgress) => {
                return Err(ResolveError::Cycle(self.aliases.cycle_path(&key)));
            }
            None => {}
        }

        self.aliases.begin(&key);
        let result = self.compute_alias(module_path, index);
        self.aliases.finish(&key, &result);
        result
    }

    fn compute_alias(
        &mut self,
        module_path: &[Ident],
        index: usize,
    ) -> Result<Option<ResolvedAlias>, ResolveError> {
        let HirItem::Alias(declared) = &self.modules.parsed(module_path).hir.items[index] else {
            unreachable!("the alias index only ever points at an alias item");
        };
        let declared: HirAlias = declared.clone();

        // Explicit generic parameters make the alias a type template: its
        // right-hand side is substituted structurally, so even a bare path
        // target is a type here rather than plain forwarding.
        if declared.generics.is_empty()
            && let AliasTarget::Path(path) = &declared.target
            && let Some(forwarded) = self.forward_alias_path(module_path, &declared, path)?
        {
            return Ok(Some(forwarded));
        }

        let origin = self.modules.definition_origin(module_path);
        let written = match &declared.target {
            AliasTarget::Path(path) => Type::Named(path.clone()),
            AliasTarget::Type(r#type) => r#type.clone(),
        };
        // An alias creates no nominal cell, so a cycle behind a pointer or any
        // other type constructor has nothing to point at. Forcing every alias
        // the target mentions -- while this one is still in progress -- is what
        // turns `alias A = *A;` into a cycle diagnostic instead of unbounded
        // expansion at the use site.
        self.force_nested_aliases(module_path, &written)?;
        Ok(Some(ResolvedAlias::Type {
            generics: declared
                .generics
                .iter()
                .map(|g| HirGenericParam {
                    ident: g.ident.clone(),
                    bounds: g.bounds.iter().map(|b| retag_origins(b, origin)).collect(),
                    default: g.default.as_ref().map(|d| retag_origins(d, origin)),
                })
                .collect(),
            r#type: retag_origins(&written, origin),
        }))
    }

    fn force_nested_aliases(
        &mut self,
        module_path: &[Ident],
        r#type: &Type,
    ) -> Result<(), ResolveError> {
        match r#type {
            Type::Named(path) | Type::Generic(path, _) => {
                let (module, name) = if path.is_unqualified() {
                    (module_path.to_vec(), path.head.clone())
                } else {
                    match self.alias_path_base(module_path, &path.head)? {
                        Some(mut absolute) => {
                            absolute.extend(path.tail.iter().cloned());
                            let (name, module) = absolute
                                .split_last()
                                .expect("a qualified path has at least two segments");
                            (module.to_vec(), name.clone())
                        }
                        None => return Ok(()),
                    }
                };
                self.declared_alias(&module, &name)?;
            }
            _ => {}
        }
        match r#type {
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => self.force_nested_aliases(module_path, inner)?,
            Type::SpecStatic(members) | Type::Generic(_, members) => {
                for member in members {
                    self.force_nested_aliases(module_path, member)?;
                }
            }
            Type::Function(f) => {
                for param in &f.params {
                    self.force_nested_aliases(module_path, &param.r#type)?;
                }
                self.force_nested_aliases(module_path, &f.return_type)?;
            }
            Type::Named(_) => {}
        }
        Ok(())
    }

    /// Turns a plain-path alias target into the absolute path it names,
    /// resolved from the alias declaration's own module. `Ok(None)` means the
    /// path names no module or top-level declaration, so it is an ordinary
    /// type expression -- `alias Count = i32;` and every other primitive,
    /// `Self`, or generic-parameter target reaches the type resolver instead
    /// of being reported as a missing item.
    fn forward_alias_path(
        &mut self,
        module_path: &[Ident],
        declared: &HirAlias,
        path: &Path,
    ) -> Result<Option<ResolvedAlias>, ResolveError> {
        let absolute = if path.is_unqualified() {
            if let Some(chained) = self.declared_alias(module_path, &path.head)? {
                return Ok(Some(chained));
            }
            match self.resolve_import_alias_entry(module_path, &path.head)? {
                Some(ImportTarget::Module(target)) => {
                    return Ok(Some(ResolvedAlias::Module(target)));
                }
                Some(ImportTarget::Item(target, _)) | Some(ImportTarget::GenericItem(target)) => {
                    target
                }
                None => module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(path.head.clone()))
                    .collect(),
            }
        } else {
            let Some(mut absolute) = self.alias_path_base(module_path, &path.head)? else {
                return Ok(None);
            };
            absolute.extend(path.tail.iter().cloned());
            absolute
        };

        if self.roots.locate(&absolute).is_ok() {
            return Ok(Some(ResolvedAlias::Module(absolute)));
        }
        let (name, target_module) = absolute
            .split_last()
            .expect("an alias target path is never empty");
        if let Some(chained) = self.declared_alias(target_module, name)? {
            return Ok(Some(chained));
        }
        if self.optional_item_index(target_module, name)?.is_none()
            && !self.module_has_macro(target_module, name)
        {
            let ambient = path
                .is_unqualified()
                .then(|| self.ambient_core_candidates(module_path, &path.head))
                .transpose()?
                .flatten();
            return match ambient {
                Some(absolute) => Ok(Some(ResolvedAlias::Item(absolute))),
                None => Ok(None),
            };
        }
        self.check_alias_target_declaration(module_path, declared, target_module, name)?;
        Ok(Some(ResolvedAlias::Item(absolute)))
    }

    /// The module a qualified alias target's head names. Unlike an ordinary
    /// qualified path, a top-level package name is admitted directly: creating
    /// a name for `std::string::String` should not also require importing it.
    fn alias_path_base(
        &mut self,
        module_path: &[Ident],
        head: &Ident,
    ) -> Result<Option<ModulePath>, ResolveError> {
        if let Some(ResolvedAlias::Module(target)) = self.declared_alias(module_path, head)? {
            return Ok(Some(target));
        }
        if let Some(ImportTarget::Module(target)) =
            self.resolve_import_alias_entry(module_path, head)?
        {
            return Ok(Some(target));
        }
        Ok(self
            .roots
            .is_known_top_level(head)
            .then(|| vec![head.clone()]))
    }

    /// Rejects a target that is not a declaration an alias may name, and a
    /// target the alias declaration site cannot see. A macro target has no
    /// item at all, which is legal: the macro namespace is separate.
    fn check_alias_target_declaration(
        &mut self,
        module_path: &[Ident],
        declared: &HirAlias,
        target_module: &[Ident],
        name: &Ident,
    ) -> Result<(), ResolveError> {
        let invalid = |kind: &'static str| ResolveError::InvalidAliasTarget {
            module: module_path.to_vec(),
            declared: declared.name.clone(),
            target: target_module
                .iter()
                .cloned()
                .chain(std::iter::once(name.clone()))
                .collect(),
            kind,
        };

        let Some(index) = self.optional_item_index(target_module, name)? else {
            return Ok(());
        };
        let item = &self.modules.parsed(target_module).hir.items[index];
        if let Some(kind) = rejected_target_kind(item) {
            return Err(invalid(kind));
        }
        let visibility = self
            .declared_visibility(target_module, name)
            .expect("just indexed by local_item_index");
        if !Self::visibility_allows(visibility, target_module, module_path) {
            return Err(ResolveError::NotVisible {
                module: target_module.to_vec(),
                item: name.clone(),
            });
        }
        Ok(())
    }

    /// The driver-side view of the one static-spec normalization rule, used by
    /// the generic-arity and generic-signature queries so they agree with the
    /// signature the analyzer will collect.
    pub(crate) fn normalized_function(
        &mut self,
        module_path: &[Ident],
        f: &omega_hir::HirFunctionDef,
    ) -> Result<omega_hir::HirFunctionDef, ResolveError> {
        omega_analyzer::generics::normalize_static_spec_params(self, module_path, f)
    }

    /// Forces every alias in a module so an alias that is never used still
    /// reports an invalid target, an inaccessible target, or a cycle.
    pub(crate) fn validate_aliases(&mut self, module_path: &[Ident]) {
        for name in self.alias_names(module_path) {
            let Err(error) = self.declared_alias(module_path, &name) else {
                continue;
            };
            let index = self
                .alias_index(module_path, &name)
                .ok()
                .flatten()
                .expect("the name came from this module's alias index");
            let (id, span) = omega_analyzer::analysis::item_id_span(
                &self.modules.parsed(module_path).hir.items[index],
            );
            self.diagnostics.error(
                module_path,
                omega_analyzer::error::AnalysisError::new(
                    id,
                    span,
                    omega_analyzer::error::AnalysisErrorKind::ModuleResolution(error),
                ),
            );
        }
    }

    pub(crate) fn alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident> {
        if self.ensure_module_indexed(module_path).is_err() {
            return vec![];
        }
        self.modules
            .index(module_path)
            .aliases
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn alias_visibility(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Option<Visibility> {
        let index = self.alias_index(module_path, name).ok()??;
        match &self.modules.parsed(module_path).hir.items[index] {
            HirItem::Alias(declared) => Some(declared.visibility),
            _ => None,
        }
    }
}
