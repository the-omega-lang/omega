use super::*;
use omega_analyzer::checked::CheckedMethodOwner;
use omega_analyzer::generics::GenericSubstitution;
use omega_analyzer::resolved_type::{FunctionNamespace, ResolvedGenericArg};
use omega_analyzer::resolver::GenericMethodTemplate;

/// A generic method declaration, together with everything the owner
/// instantiation it was reached through binds for it.
struct MethodTemplate {
    key: MethodKey,
    function: HirFunctionDef,
    site: AnalysisSite,
    /// The owner's generic arguments and `Self`, with any name the method's
    /// own generics shadow removed: an inner `T` must bind from the call, not
    /// from the owner that happens to spell a parameter the same way.
    owner_substitution: GenericSubstitution,
}

impl Driver {
    /// The single generic declaration `owner` makes under `name` in
    /// `namespace`, resolved against the owner instantiation the receiver or
    /// path already fixed.
    pub(crate) fn generic_method_template(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Option<GenericMethodTemplate>, ResolveError> {
        let Some(template) = self.find_generic_method(owner, name, namespace)? else {
            return Ok(None);
        };
        Ok(Some(GenericMethodTemplate {
            params: template
                .function
                .params
                .iter()
                .map(|p| p.r#type.clone())
                .collect(),
            return_type: template.function.return_type.clone(),
            generics: template.function.generics.clone(),
            owner_substitution: template.owner_substitution,
        }))
    }

    /// Materializes one instantiation of that template: its signature, its
    /// identity, and its body. Repeated requests for the same arguments share
    /// the one instantiation, so a call in two places links to one symbol.
    pub(crate) fn instantiate_generic_method(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
        generic_args: &[ResolvedGenericArg],
    ) -> Result<Option<ResolvedMethod>, ResolveError> {
        let Some(template) = self.find_generic_method(owner, name, namespace)? else {
            return Ok(None);
        };
        let generic_args = self.pad_generic_defaults(
            &template.key.owner.module,
            name,
            template.site,
            &template.function.generics,
            generic_args,
        )?;
        let key = MethodKey {
            generic_args,
            ..template.key.clone()
        };
        match self.items.method_instantiations.get(&key) {
            Some(MethodQueryState::Resolved(method)) => return Ok(Some(method.clone())),
            Some(MethodQueryState::Failed) => return Err(Self::method_failure(&key, name)),
            None => {}
        }

        let declared = match self.check_generic_bounds_under(
            &key.owner.module,
            template.site,
            &template.owner_substitution,
            &template.function.generics,
            &key.generic_args,
        ) {
            Some(Ok(declared)) => declared,
            Some(Err(error)) => return Err(error),
            None => return Err(self.fail_method(key, name)),
        };

        let substitution = Self::method_substitution(&template, &key.generic_args);
        let site = AnalysisSite::new(template.function.id, template.function.span);
        let signature = self.with_analyzer(&key.owner.module, &substitution, site, |analyzer| {
            analyzer.collect_function_signature(&template.function)
        });
        let Some((fn_type, annotations)) = signature.result else {
            return Err(self.fail_method(key, name));
        };
        self.diagnostics
            .record_warnings(&key.owner.module, signature.warnings);

        let decl_id = self.items.fresh_synthetic_id();
        // The owner query is where this identity's diagnostics and source
        // live; `method_identities` is what leads back to the instantiation
        // itself, which is emitted on its own.
        self.items.decl_id_owner.insert(decl_id, key.owner.clone());
        self.items.method_identities.insert(decl_id, key.clone());
        self.items
            .function_annotations
            .insert(decl_id, annotations.clone());
        let method = ResolvedMethod {
            decl_id,
            fn_type: fn_type.clone(),
            visibility: template.function.visibility,
            annotations: annotations.clone(),
            source: None,
        };
        // Cached before the body is checked: a method that instantiates
        // itself at the same arguments must find this signature instead of
        // re-entering an unfinished instantiation.
        self.items
            .method_instantiations
            .insert(key.clone(), MethodQueryState::Resolved(method.clone()));

        let bounds = self.method_bound_context(&key, template.site, &substitution, &declared);
        let run = self.with_analyzer_in(
            &key.owner.module,
            &substitution,
            &bounds,
            site,
            |analyzer| {
                analyzer.check_function_body(&template.function, &fn_type, decl_id, &annotations)
            },
        );
        if let Some(mut checked) = run.result {
            checked.generic_args = key.generic_args.clone();
            checked.method_owner = Some(CheckedMethodOwner {
                module_path: key.owner.module.clone(),
                name: key.owner.name.clone(),
                generic_args: key.owner.generic_args.clone(),
            });
            self.items.method_bodies.insert(
                key,
                CheckedBody {
                    item: CheckedItem::FunctionDefinition(checked),
                    warnings: run.warnings,
                },
            );
        }

        Ok(Some(method))
    }

    /// The bindings one method instantiation analyzes under: its own generic
    /// arguments first, so a parameter that shadows an owner parameter of the
    /// same name wins, then everything the owner instantiation supplies.
    fn method_substitution(
        template: &MethodTemplate,
        generic_args: &[ResolvedGenericArg],
    ) -> GenericSubstitution {
        let mut substitution = GenericSubstitution::zip(
            template.function.generics.iter().map(|g| &g.ident),
            generic_args,
        );
        for (name, arg) in template.owner_substitution.iter() {
            substitution.push(name.clone(), arg.clone());
        }
        substitution
    }

    fn method_bound_context(
        &mut self,
        key: &MethodKey,
        site: AnalysisSite,
        substitution: &GenericSubstitution,
        declared: &[ResolvedBound],
    ) -> Vec<ResolvedBound> {
        let keys_run = self.with_analyzer(&key.owner.module, substitution, site, |a| {
            a.expand_bound_set(site.id, site.span, declared)
        });
        self.diagnostics
            .record_warnings(&key.owner.module, keys_run.warnings);
        let keys = keys_run.result;
        self.bound_context_over(declared, &keys)
    }

    /// Records a failed instantiation, so a second call site reaching the
    /// same broken declaration references the error already reported instead
    /// of producing its own copy.
    fn fail_method(&mut self, key: MethodKey, name: &Ident) -> ResolveError {
        let failure = Self::method_failure(&key, name);
        self.items
            .method_instantiations
            .insert(key, MethodQueryState::Failed);
        failure
    }

    /// The instantiation failed for a reason an analyzer run already
    /// reported at the declaration.
    fn method_failure(key: &MethodKey, name: &Ident) -> ResolveError {
        ResolveError::ItemFailed {
            module: key.owner.module.clone(),
            item: name.clone(),
        }
    }

    /// Finds the one generic declaration an owner makes under a name. Two of
    /// them cannot be told apart before their arguments are known, so an
    /// overloaded generic name is reported rather than silently resolved to
    /// the first match.
    fn find_generic_method(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Option<MethodTemplate>, ResolveError> {
        let Some((key, self_type)) = Self::owner_item_key(owner) else {
            return Ok(None);
        };
        let index = match self.local_item_index(&key.module, &key.name) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        // Only the declarations that could match are copied out of the HIR:
        // this query runs on every type-qualified call, and an owner's other
        // declarations carry whole bodies.
        let item = &self.modules.parsed(&key.module).hir.items[index];
        let site = item_site(item);
        let (generics, functions) = match item {
            HirItem::Struct(s) => (&s.generics, &s.functions),
            HirItem::Union(u) => (&u.generics, &u.functions),
            HirItem::Enum(e) => (&e.generics, &e.functions),
            _ => return Ok(None),
        };
        let generics = generics.clone();
        let candidates: Vec<HirFunctionDef> = functions
            .iter()
            .filter(|f| {
                &f.name == name && FunctionNamespace::of_declaration(f.self_mode) == namespace
            })
            .cloned()
            .collect();

        // The *normalized* generics decide what a template is: a `spec S`
        // parameter is an anonymous bounded generic, so `f(x: spec S)` is as
        // much a template as `f<T: S>(x: T)`.
        let mut matches = Vec::new();
        for f in &candidates {
            let normalized = match self.normalized_function(&key.module, f) {
                Ok(normalized) => normalized,
                // A declaration whose written types cannot even be expanded
                // is not a template to instantiate; the ordinary member
                // lookup reports what is wrong with it.
                Err(_) if f.generics.is_empty() => continue,
                Err(error) => return Err(error),
            };
            if normalized.generics.is_empty() {
                continue;
            }
            matches.push(normalized);
        }
        let mut matches = matches.into_iter();
        let Some(function) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(ResolveError::GenericMethodOverload {
                module: key.module.clone(),
                owner: key.name.clone(),
                function: name.clone(),
            });
        }

        let mut owner_substitution = GenericSubstitution::new();
        let shadows = |name: &Ident| function.generics.iter().any(|g| &g.ident == name);
        for (param, arg) in generics.iter().zip(&key.generic_args) {
            if !shadows(&param.ident) {
                owner_substitution.push(param.ident.clone(), arg.clone());
            }
        }
        let self_name = Ident("Self".to_string());
        if !shadows(&self_name) {
            owner_substitution.push_type(self_name, self_type);
        }

        Ok(Some(MethodTemplate {
            key: MethodKey {
                owner: key,
                method: function.id,
                generic_args: Vec::new(),
            },
            site,
            function,
            owner_substitution,
        }))
    }

    /// The item query a resolved aggregate type came from, which is also the
    /// context its own declarations are analyzed in.
    fn owner_item_key(owner: &ResolvedType) -> Option<(ItemKey, ResolvedType)> {
        let key = match owner {
            ResolvedType::Struct(cell) => {
                let owner = cell.borrow();
                ItemKey::new(&owner.module_path, &owner.name, &owner.generic_args)
            }
            ResolvedType::Union(cell) => {
                let owner = cell.borrow();
                ItemKey::new(&owner.module_path, &owner.name, &owner.generic_args)
            }
            ResolvedType::Enum { cell, .. } => {
                let owner = cell.borrow();
                ItemKey::new(&owner.module_path, &owner.name, &owner.generic_args)
            }
            _ => return None,
        };
        // A refined enum receiver still declares its functions on the enum
        // itself, so the owner type is the unrefined one.
        let self_type = match owner {
            ResolvedType::Enum { cell, .. } => ResolvedType::Enum {
                cell: cell.clone(),
                variant: None,
            },
            other => other.clone(),
        };
        Some((key, self_type))
    }
}
