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
        self.resolved
            .insert(key.clone(), AliasQueryState::InProgress);
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

/// Where an alias target path points, once resolved from the declaration
/// site.
enum AliasTargetSite {
    Module(ModulePath),
    Declaration {
        module: ModulePath,
        name: Ident,
        /// The head was admitted as a bare top-level package identity, a
        /// rule that exists only for alias targets. A path written this way
        /// resolves differently at an ordinary use site, so it is rebound
        /// before expansion (see `rebind_alias_target`).
        top_level_head: bool,
    },
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

    /// The accessor-aware alias lookup every semantic use site goes
    /// through. `declared_alias` below is the raw memoized graph query it
    /// gates: that one answers *what* an alias names, this one answers
    /// whether `accessor` may ask at all.
    pub(crate) fn visible_alias(
        &mut self,
        accessor: &[Ident],
        alias_module: &[Ident],
        name: &Ident,
        bypass_visibility: bool,
    ) -> Result<Option<ResolvedAlias>, ResolveError> {
        if self.alias_index(alias_module, name)?.is_none() {
            return Ok(None);
        }
        if !bypass_visibility {
            let visibility = self
                .alias_visibility(alias_module, name)
                .expect("just indexed by alias_index");
            if !Self::visibility_allows(visibility, alias_module, accessor) {
                return Err(ResolveError::NotVisible {
                    module: alias_module.to_vec(),
                    item: name.clone(),
                });
            }
        }
        self.declared_alias(alias_module, name)
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
        // An alias-owned generic parameter is a legal opaque placeholder
        // everywhere in the target, its bounds, and its defaults; every other
        // referenced type/spec/path must resolve and be visible from this
        // declaration site, and every nested alias is forced -- while this
        // one is still in progress -- so a cycle (direct or behind a pointer
        // or any other type constructor) is reported here instead of causing
        // unbounded expansion at a use site. This runs even if the alias is
        // never used: an alias declaration is not just a lazy forwarding
        // rule, its target must be legal on its own.
        let placeholders: Vec<Ident> = declared.generics.iter().map(|g| g.ident.clone()).collect();
        for param in &declared.generics {
            for bound in &param.bounds {
                self.validate_alias_target(module_path, &declared, &placeholders, bound)?;
            }
            if let Some(default) = &param.default {
                self.validate_alias_target(module_path, &declared, &placeholders, default)?;
            }
        }
        self.validate_alias_target(module_path, &declared, &placeholders, &written)?;
        let generics = declared
            .generics
            .iter()
            .map(|g| HirGenericParam {
                ident: g.ident.clone(),
                bounds: g
                    .bounds
                    .iter()
                    .map(|b| self.rebind_alias_target(module_path, &placeholders, b, origin))
                    .collect(),
                default: g
                    .default
                    .as_ref()
                    .map(|d| self.rebind_alias_target(module_path, &placeholders, d, origin)),
            })
            .collect::<Vec<_>>();
        let r#type = self.rebind_alias_target(module_path, &placeholders, &written, origin);
        Ok(Some(ResolvedAlias::Type { generics, r#type }))
    }

    /// Symbolically validates a structural alias target -- or one of its own
    /// generic bounds/defaults -- without materializing anything.
    ///
    /// A name in `placeholders` is the alias's own generic parameter: always
    /// a legal opaque reference, never given a fabricated resolution. Every
    /// other name must exist, be a type or spec (this is type syntax, not
    /// the bare-path namespace an ordinary forwarding alias may use), be
    /// visible from `module_path` -- the declaration site -- and be applied
    /// with a legal number of generic arguments. Every nested alias is
    /// forced so a cycle surfaces here rather than at a first use.
    fn validate_alias_target(
        &mut self,
        module_path: &[Ident],
        declared: &HirAlias,
        placeholders: &[Ident],
        r#type: &Type,
    ) -> Result<(), ResolveError> {
        match r#type {
            Type::Named(path) | Type::Generic(path, _) => {
                let args = match r#type {
                    Type::Generic(_, args) => args.as_slice(),
                    _ => &[],
                };
                self.validate_alias_target_path(
                    module_path,
                    declared,
                    placeholders,
                    path,
                    args,
                    false,
                )?;
            }
            _ => {}
        }
        match r#type {
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => {
                self.validate_alias_target(module_path, declared, placeholders, inner)?
            }
            Type::Generic(_, args) => {
                for arg in args {
                    self.validate_alias_target(module_path, declared, placeholders, arg)?;
                }
            }
            // Every member of a conjunction is a spec reference, so a type
            // or function whose name merely exists does not pass here.
            Type::SpecStatic(members) => {
                for member in members {
                    let (path, args) = match member {
                        Type::Named(path) => (path, &[][..]),
                        Type::Generic(path, args) => (path, args.as_slice()),
                        _ => {
                            return Err(ResolveError::InvalidAliasTarget {
                                module: module_path.to_vec(),
                                declared: declared.name.clone(),
                                target: vec![declared.name.clone()],
                                kind: "a non-spec type as a spec conjunction member,",
                                type_position: true,
                            });
                        }
                    };
                    self.validate_alias_target_path(
                        module_path,
                        declared,
                        placeholders,
                        path,
                        args,
                        true,
                    )?;
                    for arg in args {
                        self.validate_alias_target(module_path, declared, placeholders, arg)?;
                    }
                }
            }
            Type::Function(f) => {
                for param in &f.params {
                    self.validate_alias_target(module_path, declared, placeholders, &param.r#type)?;
                }
                self.validate_alias_target(module_path, declared, placeholders, &f.return_type)?;
            }
            Type::Named(_) => {}
        }
        Ok(())
    }

    /// One named reference inside a structural alias target: it must name a
    /// type (or, in `spec` conjunction position, a spec), and be applied
    /// with an argument count its declared generics accept.
    fn validate_alias_target_path(
        &mut self,
        module_path: &[Ident],
        declared: &HirAlias,
        placeholders: &[Ident],
        path: &Path,
        args: &[Type],
        expect_spec: bool,
    ) -> Result<(), ResolveError> {
        let invalid = |target: Vec<Ident>, kind: &'static str| ResolveError::InvalidAliasTarget {
            module: module_path.to_vec(),
            declared: declared.name.clone(),
            target,
            kind,
            type_position: true,
        };

        // An alias's own generic parameter stands for a type supplied later;
        // it is not itself a template, so it takes no arguments.
        if path.is_unqualified() && placeholders.contains(&path.head) {
            return Self::check_generic_arity(module_path, &path.head, &[], args.len());
        }

        // `str` is a builtin name too, legal only directly behind a pointer
        // (`*str`) -- that pointer-position rule is enforced by ordinary type
        // resolution at every use site, exactly as it is for a literal
        // (non-aliased) `*str`, so this pass only has to recognize the name.
        if path.is_unqualified()
            && (omega_analyzer::BUILTIN_TYPE_NAMES.contains(&path.head.as_ref())
                || path.head.as_ref() == "str"
                || path.head.as_ref() == "Self")
        {
            return Self::check_generic_arity(module_path, &path.head, &[], args.len());
        }

        let site = match self.alias_target_site(module_path, path)? {
            Some(site) => site,
            None => return Err(ResolveError::UnknownModule(path.segments())),
        };
        let (module, name) = match site {
            AliasTargetSite::Module(target) => return Err(invalid(target, "the module")),
            AliasTargetSite::Declaration { module, name, .. } => (module, name),
        };
        let absolute = || {
            module
                .iter()
                .cloned()
                .chain(std::iter::once(name.clone()))
                .collect::<Vec<_>>()
        };

        // Forcing the nested alias while this one is still in progress is
        // what turns a cycle behind a pointer, a generic argument, a default
        // or a bound into the same deterministic cycle report a direct one
        // gets.
        if let Some(nested) = self.visible_alias(module_path, &module, &name, false)? {
            return match nested {
                ResolvedAlias::Module(target) => Err(invalid(target, "the module")),
                ResolvedAlias::Overloads { absolute, .. } => {
                    Err(invalid(absolute, "the overload set"))
                }
                ResolvedAlias::Type { generics, .. } => {
                    Self::check_generic_arity(&module, &name, &generics, args.len())
                }
                ResolvedAlias::Item(target) => {
                    let (target_name, target_module) = target
                        .split_last()
                        .expect("an alias item target is never empty");
                    let (target_name, target_module) =
                        (target_name.clone(), target_module.to_vec());
                    self.check_type_position_declaration(
                        module_path,
                        declared,
                        &target_module,
                        &target_name,
                        expect_spec,
                    )?;
                    let generics = self.item_generics(&target_module, &target_name)?;
                    Self::check_generic_arity(&target_module, &target_name, &generics, args.len())
                }
            };
        }

        if self.optional_item_index(&module, &name)?.is_none() {
            if self.module_has_macro(&module, &name) {
                return Err(invalid(absolute(), "the macro"));
            }
            let ambient = path
                .is_unqualified()
                .then(|| self.ambient_core_candidates(module_path, &path.head))
                .transpose()?
                .flatten();
            let Some(ambient) = ambient else {
                return Err(ResolveError::UnknownItem { module, item: name });
            };
            let (ambient_name, ambient_module) = ambient
                .split_last()
                .expect("an ambient candidate path is never empty");
            let (ambient_name, ambient_module) = (ambient_name.clone(), ambient_module.to_vec());
            self.check_type_position_declaration(
                module_path,
                declared,
                &ambient_module,
                &ambient_name,
                expect_spec,
            )?;
            let generics = self.item_generics(&ambient_module, &ambient_name)?;
            return Self::check_generic_arity(
                &ambient_module,
                &ambient_name,
                &generics,
                args.len(),
            );
        }

        self.check_type_position_declaration(module_path, declared, &module, &name, expect_spec)?;
        let generics = self.item_generics(&module, &name)?;
        Self::check_generic_arity(&module, &name, &generics, args.len())
    }

    /// An explicit argument list must fit the declared parameters, and every
    /// parameter left unsupplied must have a default. This is the same rule
    /// a use site applies, checked here so a malformed application is
    /// reported even when the alias is never used.
    fn check_generic_arity(
        module: &[Ident],
        name: &Ident,
        generics: &[HirGenericParam],
        found: usize,
    ) -> Result<(), ResolveError> {
        let required = generics.iter().filter(|g| g.default.is_none()).count();
        if found > generics.len() || found < required {
            return Err(ResolveError::GenericArgCountMismatch {
                module: module.to_vec(),
                item: name.clone(),
                expected: generics.len(),
                found,
            });
        }
        Ok(())
    }

    /// Rejects a declaration that is not a type -- or, in `spec` conjunction
    /// position, not a spec -- and one the alias declaration site cannot
    /// see. Type syntax is narrower than the bare-path alias namespace: a
    /// function or module an ordinary forwarding alias may name is still not
    /// a type.
    fn check_type_position_declaration(
        &mut self,
        module_path: &[Ident],
        declared: &HirAlias,
        target_module: &[Ident],
        name: &Ident,
        expect_spec: bool,
    ) -> Result<(), ResolveError> {
        let Some(index) = self.optional_item_index(target_module, name)? else {
            return Err(ResolveError::UnknownItem {
                module: target_module.to_vec(),
                item: name.clone(),
            });
        };
        let item = &self.modules.parsed(target_module).hir.items[index];
        let rejected = match item {
            HirItem::Spec(_) => None,
            _ if expect_spec => Some("the non-spec declaration"),
            HirItem::Struct(_) | HirItem::Enum(_) | HirItem::Union(_) => None,
            HirItem::FunctionDefinition(_) | HirItem::ForeignFunction(_) => Some("the function"),
            HirItem::ForeignBinding(binding) => match binding.r#type {
                Type::Function(_) => Some("the function"),
                _ => Some("the global"),
            },
            HirItem::Declaration { .. } | HirItem::DeclarationWithInit { .. } => Some("the global"),
            HirItem::Walrus { walrus, .. } if walrus.comp => Some("the compile-time value"),
            HirItem::Walrus { .. } => Some("the global"),
            HirItem::Gap(_) => Some("the gap"),
            HirItem::Alias(_)
            | HirItem::Glue(_)
            | HirItem::Conform(_)
            | HirItem::Primitive(_)
            | HirItem::Import(_) => {
                unreachable!("the item index never names an alias or an unnamed item")
            }
        };
        if let Some(kind) = rejected {
            return Err(ResolveError::InvalidAliasTarget {
                module: module_path.to_vec(),
                declared: declared.name.clone(),
                target: target_module
                    .iter()
                    .cloned()
                    .chain(std::iter::once(name.clone()))
                    .collect(),
                kind,
                type_position: true,
            });
        }
        let visibility = self
            .declared_visibility(target_module, name)
            .expect("just indexed by optional_item_index");
        if !Self::visibility_allows(visibility, target_module, module_path) {
            return Err(ResolveError::NotVisible {
                module: target_module.to_vec(),
                item: name.clone(),
            });
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
        let absolute = if let Some(anchored) = Driver::resolve_explicit_anchor(module_path, path) {
            anchored?
        } else if path.tail.is_empty() {
            if let Some(chained) = self.declared_alias(module_path, &path.head)? {
                return Ok(Some(chained));
            }
            match self.resolve_import_alias_entry(module_path, &path.head) {
                Ok(Some(ImportTarget::Module(target))) => {
                    return Ok(Some(ResolvedAlias::Module(target)));
                }
                Ok(Some(ImportTarget::Item(target, _))) => target,
                Ok(Some(ImportTarget::ItemPath(access))) => access.absolute,
                Ok(None) => module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(path.head.clone()))
                    .collect(),
                // A macro has no item, so an import that binds only a macro
                // cannot resolve as one. The macro namespace is separate and
                // is a legal alias target, subject to the same gate a direct
                // invocation of that binding would pass.
                Err(error) => self.imported_macro_target(module_path, &path.head, error)?,
            }
        } else {
            let Some((mut absolute, _)) = self.alias_path_base(module_path, &path.head)? else {
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
        // An intermediate alias in a chain is its own visibility gate: this
        // alias's own declaration site must be able to see it before its
        // target is followed, exactly like any other named declaration
        // reference -- a hidden link may not be smuggled through simply
        // because the alias that follows it happens to be exposed.
        if let Some(chained) = self.visible_alias(module_path, target_module, name, false)? {
            return Ok(Some(chained));
        }
        // An overload group is one name for several declarations, each with
        // its own visibility. The alias freezes exactly the ones its own
        // declaration site can name here, once, rather than leaving each
        // caller to re-derive a set from rights it does not have.
        if let Some(candidates) = self.visible_overload_decls(module_path, target_module, name)? {
            return Ok(Some(ResolvedAlias::Overloads {
                absolute,
                candidates,
            }));
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

    /// The `decl_id`s under `name` in `target_module` that `accessor` may
    /// name. `Ok(None)` means there is no overload group there at all; an
    /// existing group none of whose candidates is nameable answers exactly
    /// as a single inaccessible declaration would.
    pub(crate) fn visible_overload_decls(
        &mut self,
        accessor: &[Ident],
        target_module: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<omega_hir::HirId>>, ResolveError> {
        self.ensure_module_indexed(target_module)?;
        let Some(indices) = self
            .modules
            .index(target_module)
            .overloads
            .get(name)
            .cloned()
        else {
            return Ok(None);
        };
        let hir = self.modules.hir(target_module);
        let visible: Vec<omega_hir::HirId> = indices
            .iter()
            .filter_map(|&index| {
                let HirItem::FunctionDefinition(f) = &hir.items[index] else {
                    unreachable!("only a function is ever recorded as an overload candidate");
                };
                Self::visibility_allows(f.visibility, target_module, accessor).then_some(f.id)
            })
            .collect();
        if visible.is_empty() {
            return Err(ResolveError::NotVisible {
                module: target_module.to_vec(),
                item: name.clone(),
            });
        }
        Ok(Some(visible))
    }

    /// The module a qualified alias target's head names, and whether it was
    /// admitted as a bare top-level package identity. Unlike an ordinary
    /// qualified path, a top-level package name is admitted directly here:
    /// creating a name for `std::string::String` should not also require
    /// importing it.
    fn alias_path_base(
        &mut self,
        module_path: &[Ident],
        head: &Ident,
    ) -> Result<Option<(ModulePath, bool)>, ResolveError> {
        if let Some(ResolvedAlias::Module(target)) = self.declared_alias(module_path, head)? {
            return Ok(Some((target, false)));
        }
        if let Some(ImportTarget::Module(target)) =
            self.resolve_import_alias_entry(module_path, head)?
        {
            return Ok(Some((target, false)));
        }
        Ok(self
            .roots
            .is_known_top_level(head)
            .then(|| (vec![head.clone()], true)))
    }

    /// Where an alias target path points, resolved once from the alias
    /// declaration site. Declaration validation and structural expansion
    /// both go through here, so a target cannot validate under one set of
    /// rules and then be re-resolved under another at a use site.
    /// `Ok(None)` means the path names no reachable module or declaration,
    /// which for a bare-path alias means the target is an ordinary type
    /// expression instead.
    fn alias_target_site(
        &mut self,
        module_path: &[Ident],
        path: &Path,
    ) -> Result<Option<AliasTargetSite>, ResolveError> {
        let (absolute, top_level_head) =
            if let Some(anchored) = Driver::resolve_explicit_anchor(module_path, path) {
                (anchored?, false)
            } else if path.tail.is_empty() {
                // An import binds the same name a local declaration would,
                // so an unqualified target may be either.
                match self.resolve_import_alias_entry(module_path, &path.head)? {
                    Some(ImportTarget::Module(target)) => {
                        return Ok(Some(AliasTargetSite::Module(target)));
                    }
                    Some(ImportTarget::Item(target, _)) => (target, false),
                    Some(ImportTarget::ItemPath(access)) => (access.absolute, false),
                    None => (
                        module_path
                            .iter()
                            .cloned()
                            .chain(std::iter::once(path.head.clone()))
                            .collect(),
                        false,
                    ),
                }
            } else {
                let Some((base, top_level_head)) = self.alias_path_base(module_path, &path.head)?
                else {
                    return Ok(None);
                };
                (
                    base.into_iter().chain(path.tail.iter().cloned()).collect(),
                    top_level_head,
                )
            };
        if self.roots.locate(&absolute).is_ok() {
            return Ok(Some(AliasTargetSite::Module(absolute)));
        }
        let (name, module) = absolute
            .split_last()
            .expect("an alias target path is never empty");
        Ok(Some(AliasTargetSite::Declaration {
            module: module.to_vec(),
            name: name.clone(),
            top_level_head,
        }))
    }

    /// Rebinds a validated structural alias target so it means the same
    /// thing wherever it is later expanded.
    ///
    /// Every path is tagged with the alias declaration module's origin, the
    /// same provenance mechanism macro expansion uses, so the target keeps
    /// resolving at its declaration site. A path admitted as a bare
    /// top-level package identity gets more: no ordinary use site has that
    /// rule, so the path is rewritten to name its target directly from the
    /// target's own module. Without this, `alias W<T> = main::helper::H<T>;`
    /// would validate and then fail at every use with `ModuleNotImported`.
    fn rebind_alias_target(
        &mut self,
        module_path: &[Ident],
        placeholders: &[Ident],
        ty: &Type,
        origin: Origin,
    ) -> Type {
        let rebind_path = |driver: &mut Self, path: &Path| -> Path {
            if !(path.is_unqualified() && placeholders.contains(&path.head))
                && let Ok(Some(AliasTargetSite::Declaration {
                    module,
                    name,
                    top_level_head: true,
                })) = driver.alias_target_site(module_path, path)
            {
                return Path {
                    anchor: None,
                    head: name,
                    tail: Vec::new(),
                    origin: driver.modules.definition_origin(&module),
                };
            }
            Path {
                anchor: path.anchor,
                head: path.head.clone(),
                tail: path.tail.clone(),
                origin,
            }
        };
        let recur = |driver: &mut Self, inner: &Type| {
            driver.rebind_alias_target(module_path, placeholders, inner, origin)
        };
        match ty {
            Type::Named(path) => Type::Named(rebind_path(self, path)),
            Type::Generic(path, args) => {
                let path = rebind_path(self, path);
                let args = args.iter().map(|a| recur(self, a)).collect();
                Type::Generic(path, args)
            }
            Type::Pointer(inner, mutable) => Type::Pointer(Box::new(recur(self, inner)), *mutable),
            Type::InferredArray(inner) => Type::InferredArray(Box::new(recur(self, inner))),
            Type::UnknownSizeArray(inner) => Type::UnknownSizeArray(Box::new(recur(self, inner))),
            Type::SizedArray(inner, size) => {
                Type::SizedArray(Box::new(recur(self, inner)), size.clone())
            }
            Type::SpecStatic(members) => {
                Type::SpecStatic(members.iter().map(|m| recur(self, m)).collect())
            }
            Type::Function(f) => Type::Function(FunctionType {
                params: f
                    .params
                    .iter()
                    .map(|p| Param {
                        r#type: recur(self, &p.r#type),
                        ..p.clone()
                    })
                    .collect(),
                return_type: Box::new(recur(self, &f.return_type)),
                is_variadic: f.is_variadic,
                self_mode: f.self_mode,
                convention: f.convention.clone(),
            }),
        }
    }

    /// The absolute path of a macro bound in `module_path` by an import.
    /// `fallback` is the item-namespace failure to report when the name is
    /// not an imported macro after all.
    fn imported_macro_target(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        fallback: ResolveError,
    ) -> Result<ModulePath, ResolveError> {
        let Some((target, reveal)) = self.import_entry(module_path, name)? else {
            return Err(fallback);
        };
        let Some((macro_name, macro_module)) = target.split_last() else {
            return Err(fallback);
        };
        let (macro_name, macro_module) = (macro_name.clone(), macro_module.to_vec());
        let Some(visibility) = self.macro_visibility(&macro_module, &macro_name) else {
            return Err(fallback);
        };
        if !reveal && !Self::visibility_allows(visibility, &macro_module, module_path) {
            return Err(ResolveError::NotVisible {
                module: macro_module,
                item: macro_name,
            });
        }
        Ok(target)
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
            type_position: false,
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
