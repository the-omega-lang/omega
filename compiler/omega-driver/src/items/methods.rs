use super::*;
use omega_analyzer::checked::{CheckedMethodOwner, ConformanceOwner};
use omega_analyzer::generics::GenericSubstitution;
use omega_analyzer::resolved_type::{FunctionNamespace, ResolvedBound, ResolvedGenericArg};
use omega_analyzer::resolver::{GenericMethodTemplate, OverloadCandidate, OverloadCandidates};

/// A generic method declaration, together with everything the owner
/// instantiation it was reached through binds for it.
struct MethodTemplate {
    key: ItemKey,
    function: HirFunctionDef,
    site: AnalysisSite,
    /// The owner's generic arguments and `Self`, with any name the method's
    /// own generics shadow removed: an inner `T` must bind from the call, not
    /// from the owner that happens to spell a parameter the same way.
    owner_substitution: GenericSubstitution,
    conformance_owner: Option<ConformanceOwner>,
    enclosing_bounds: Vec<ResolvedBound>,
}

impl Driver {
    pub(crate) fn collect_method_overloads(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<OverloadCandidates, ResolveError> {
        let mut candidates: Vec<_> = owner
            .candidates_in(namespace, name)
            .unwrap_or_default()
            .into_iter()
            .map(|method| OverloadCandidate {
                decl_id: method.decl_id,
                signature: omega_analyzer::resolver::OverloadSignature::Concrete(method.fn_type),
                visibility: method.visibility,
                declaring_module: method.declaring_module,
            })
            .collect();
        let templates = self.find_generic_methods(owner, name, namespace)?;
        if candidates.len() + templates.len() < 2 {
            return Ok(candidates);
        }
        candidates.extend(self.template_candidates(templates)?);
        Ok(candidates)
    }

    /// The generic declarations an owner makes under a name, as candidates an
    /// uncalled reference can select between. A lone template is included:
    /// unlike a call, a value reference has no other path to it.
    pub(crate) fn collect_method_value_templates(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<OverloadCandidates, ResolveError> {
        let templates = self.find_generic_methods(owner, name, namespace)?;
        self.template_candidates(templates)
    }

    fn template_candidates(
        &mut self,
        templates: Vec<MethodTemplate>,
    ) -> Result<OverloadCandidates, ResolveError> {
        let mut candidates = Vec::with_capacity(templates.len());
        for template in templates {
            let run = self.with_analyzer(
                template.key.module(),
                &template.owner_substitution,
                template.site,
                |analyzer| analyzer.overload_template(&template.function),
            );
            let pattern = run.result.ok_or_else(|| template.key.failed())?;
            let decl_id = self.items.fresh_synthetic_id();
            let declaring_module = template.key.module().clone();
            self.items.decl_id_owner.insert(decl_id, template.key);
            candidates.push(OverloadCandidate {
                decl_id,
                signature: omega_analyzer::resolver::OverloadSignature::Template(pattern),
                visibility: template.function.visibility,
                declaring_module,
            });
        }
        Ok(candidates)
    }

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
        self.instantiate_method_template(template, generic_args)
            .map(Some)
    }

    pub(crate) fn instantiate_method_overload(
        &mut self,
        key: &ItemKey,
        generic_args: &[Option<ResolvedGenericArg>],
    ) -> Result<ResolvedMethod, ResolveError> {
        let template = self.method_template_for_key(key);
        let generic_args = self.complete_overload_arguments(
            key,
            &template.function,
            &template.owner_substitution,
            generic_args,
        )?;
        self.instantiate_method_template(template, &generic_args)
    }

    fn instantiate_method_template(
        &mut self,
        template: MethodTemplate,
        generic_args: &[ResolvedGenericArg],
    ) -> Result<ResolvedMethod, ResolveError> {
        let generic_args = self.pad_generic_defaults(
            template.key.module(),
            &template.function.name,
            template.site,
            &template.function.generics,
            generic_args,
        )?;
        let key = ItemKey {
            generic_args,
            ..template.key.clone()
        };
        let visibility = template.function.visibility;
        let item =
            self.ensure_item_query(&key, visibility, ResolveItemOptions::INDIRECT, |driver| {
                driver.compute_method_signature(&key, &template)
            })?;
        self.ensure_item_body(&key);
        let ResolvedItem::Value {
            decl_id,
            r#type: ResolvedType::Function(fn_type),
            ..
        } = item
        else {
            unreachable!("a method query resolves a function");
        };
        Ok(ResolvedMethod {
            decl_id,
            fn_type,
            visibility,
            declaring_module: key.module().to_vec(),
            annotations: self.items.function_annotations[&decl_id].clone(),
            source: None,
        })
    }

    fn compute_method_signature(
        &mut self,
        key: &ItemKey,
        template: &MethodTemplate,
    ) -> Result<ResolvedItem, ResolveError> {
        let declared = match self.check_generic_bounds_under(
            key.module(),
            template.site,
            &template.owner_substitution,
            &template.function.generics,
            &key.generic_args,
        ) {
            Some(Ok(declared)) => declared,
            Some(Err(error)) => return Err(error),
            None => return Err(key.failed()),
        };
        self.items.declared_bounds.insert(key.clone(), declared);
        let substitution = Self::method_substitution(template, &key.generic_args);
        let site = AnalysisSite::new(template.function.id, template.function.span);
        let signature = self.with_analyzer(key.module(), &substitution, site, |analyzer| {
            analyzer.collect_function_signature(&template.function)
        });
        self.diagnostics
            .record_warnings(key.module(), signature.warnings);
        let (fn_type, annotations) = signature.result.ok_or_else(|| key.failed())?;
        let decl_id = self.items.identity_for(key, template.function.id);
        self.items.function_annotations.insert(decl_id, annotations);
        Ok(ResolvedItem::Value {
            decl_id,
            r#type: ResolvedType::Function(fn_type),
            storage: Storage::Function,
            mutable: false,
        })
    }

    pub(crate) fn check_method_body(&mut self, key: &ItemKey) -> Option<CheckedBody> {
        let template = self.method_template_for_key(key);
        let ResolvedItem::Value {
            decl_id,
            r#type: ResolvedType::Function(fn_type),
            ..
        } = self.items.expect_resolved(key).clone()
        else {
            unreachable!("a method query resolves a function");
        };
        let annotations = self.items.function_annotations[&decl_id].clone();
        let descriptor = if template.function.generics.is_empty() {
            None
        } else {
            Some(self.template_descriptor(key, &template.function, &template.owner_substitution)?)
        };
        let substitution = Self::method_substitution(&template, &key.generic_args);
        let declared = self
            .items
            .declared_bounds
            .get(key)
            .cloned()
            .unwrap_or_default();
        let mut bounds = self.method_bound_context(key, template.site, &substitution, &declared);
        bounds.extend(template.enclosing_bounds);
        let site = AnalysisSite::new(template.function.id, template.function.span);
        let run = self.with_analyzer_in(key.module(), &substitution, &bounds, site, |analyzer| {
            analyzer.check_function_body(&template.function, &fn_type, decl_id, &annotations)
        });
        let mut checked = run.result?;
        checked.generic_args = key.generic_args.clone();
        checked.template = descriptor;
        if let Some(owner) = template.conformance_owner {
            checked.conformance_owner = Some(owner);
        } else {
            let owner = key.owner().expect("a method has an owner");
            checked.method_owner = Some(CheckedMethodOwner {
                module_path: owner.module().clone(),
                name: owner.name.clone(),
                generic_args: owner.generic_args.clone(),
            });
        }
        Some(CheckedBody {
            item: CheckedItem::FunctionDefinition(checked),
            warnings: run.warnings,
        })
    }

    /// The same declaration a preparation query needs, without the parts
    /// only instantiation uses.
    pub(crate) fn method_declaration_for_key(
        &mut self,
        key: &ItemKey,
    ) -> crate::items::preparation::GenericDeclaration {
        let template = self.method_template_for_key(key);
        crate::items::preparation::GenericDeclaration {
            key: template.key,
            function: template.function,
            site: template.site,
            enclosing: template.owner_substitution,
        }
    }

    /// The one generic function `owner` declares under `name`, as a
    /// preparation target.
    pub(crate) fn generic_method_declaration(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Option<crate::items::preparation::GenericDeclaration>, ResolveError> {
        Ok(self
            .find_generic_method(owner, name, namespace)?
            .map(|template| crate::items::preparation::GenericDeclaration {
                key: template.key,
                function: template.function,
                site: template.site,
                enclosing: template.owner_substitution,
            }))
    }

    fn method_template_for_key(&mut self, key: &ItemKey) -> MethodTemplate {
        let owner = key.owner().expect("a method query has an owner");
        let (target, namespace) = if let Some(target) = self.items.cells.resolved_type(owner) {
            let hir = self.modules.hir(owner.module());
            let functions = match &hir.items[owner.disambiguator] {
                HirItem::Struct(s) => &s.functions,
                HirItem::Enum(e) => &e.functions,
                HirItem::Union(u) => &u.functions,
                _ => unreachable!("nominal owners declare methods"),
            };
            (
                target,
                FunctionNamespace::of_declaration(functions[key.disambiguator].self_mode),
            )
        } else {
            let entry = self
                .conformances
                .entries
                .iter()
                .find(|entry| self.conformance_method_key(entry) == *owner)
                .expect("a resolved conformance method has a registered owner");
            (
                entry.target.clone(),
                FunctionNamespace::of_declaration(entry.functions[key.disambiguator].self_mode),
            )
        };
        let template = self
            .find_generic_methods(&target, &key.name, namespace)
            .expect("a resolved method template remains valid")
            .into_iter()
            .find(|template| {
                template.key.scope == key.scope && template.key.disambiguator == key.disambiguator
            })
            .expect("a resolved method has a template");
        debug_assert_eq!(&template.key.scope, &key.scope);
        debug_assert_eq!(template.key.disambiguator, key.disambiguator);
        template
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
        key: &ItemKey,
        site: AnalysisSite,
        substitution: &GenericSubstitution,
        declared: &[ResolvedBound],
    ) -> Vec<ResolvedBound> {
        let keys_run = self.with_analyzer(key.module(), substitution, site, |a| {
            a.expand_bound_set(site.id, site.span, declared)
        });
        self.diagnostics
            .record_warnings(key.module(), keys_run.warnings);
        let keys = keys_run.result;
        self.bound_context_over(declared, &keys)
    }

    fn find_generic_method(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Option<MethodTemplate>, ResolveError> {
        Ok(self
            .find_generic_methods(owner, name, namespace)?
            .into_iter()
            .next())
    }

    fn find_generic_methods(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Vec<MethodTemplate>, ResolveError> {
        let Some((key, self_type)) = self.owner_item_key(owner) else {
            return self.find_generic_conformance_methods(owner, name, namespace);
        };
        let index = match self.local_item_index(key.module(), &key.name) {
            Ok(index) => index,
            Err(_) => return Ok(Vec::new()),
        };
        // Only the declarations that could match are copied out of the HIR:
        // this query runs on every type-qualified call, and an owner's other
        // declarations carry whole bodies.
        let item = &self.modules.parsed(key.module()).hir.items[index];
        let site = item_site(item);
        let (generics, functions) = match item {
            HirItem::Struct(s) => (&s.generics, &s.functions),
            HirItem::Union(u) => (&u.generics, &u.functions),
            HirItem::Enum(e) => (&e.generics, &e.functions),
            _ => return self.find_generic_conformance_methods(owner, name, namespace),
        };
        let generics = generics.clone();
        let candidates: Vec<(usize, HirFunctionDef)> = functions
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                &f.name == name && FunctionNamespace::of_declaration(f.self_mode) == namespace
            })
            .map(|(index, function)| (index, function.clone()))
            .collect();

        // The *normalized* generics decide what a template is: a `spec S`
        // parameter is an anonymous bounded generic, so `f(x: spec S)` is as
        // much a template as `f<T: S>(x: T)`.
        let mut matches = Vec::new();
        for (index, f) in &candidates {
            let normalized = match self.normalized_function(key.module(), f) {
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
            matches.push((*index, normalized));
        }
        if matches.is_empty() {
            return self.find_generic_conformance_methods(owner, name, namespace);
        }
        let mut templates = Vec::new();
        for (index, function) in matches {
            let mut owner_substitution = GenericSubstitution::new();
            let shadows = |name: &Ident| function.generics.iter().any(|g| &g.ident == name);
            for (param, arg) in generics.iter().zip(&key.generic_args) {
                if !shadows(&param.ident) {
                    owner_substitution.push(param.ident.clone(), arg.clone());
                }
            }
            let self_name = Ident("Self".to_string());
            if !shadows(&self_name) {
                owner_substitution.push_type(self_name, self_type.clone());
            }

            templates.push(MethodTemplate {
                key: ItemKey::member(key.clone(), function.name.clone(), index),
                site,
                function,
                owner_substitution,
                conformance_owner: None,
                enclosing_bounds: Vec::new(),
            });
        }
        Ok(templates)
    }

    fn find_generic_conformance_methods(
        &mut self,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> Result<Vec<MethodTemplate>, ResolveError> {
        let mut candidates = Vec::new();
        for entry in self.conformances_for_type(owner) {
            for (index, (function, method_id)) in
                entry.functions.iter().zip(&entry.method_ids).enumerate()
            {
                if entry.templates.contains(method_id)
                    && function.name == *name
                    && FunctionNamespace::of_declaration(function.self_mode) == namespace
                {
                    candidates.push((entry.clone(), index, function.clone()));
                }
            }
        }
        let mut templates = Vec::new();
        for (entry, index, mut function) in candidates {
            let shadows = |name: &Ident| {
                function
                    .generics
                    .iter()
                    .any(|generic| &generic.ident == name)
            };
            let mut owner_substitution = GenericSubstitution::new();
            for (bound, arg) in entry.substitution.iter() {
                if !shadows(bound) {
                    owner_substitution.push(bound.clone(), arg.clone());
                }
            }
            if let Some((_, requirement)) = entry
                .spec
                .borrow()
                .functions
                .iter()
                .find(|(requirement, _)| *requirement == function.name)
            {
                function.visibility = requirement.visibility;
            }
            templates.push(MethodTemplate {
                key: ItemKey::member(
                    self.conformance_method_key(&entry),
                    function.name.clone(),
                    index,
                ),
                site: AnalysisSite::new(function.id, function.span),
                function,
                owner_substitution,
                conformance_owner: Some(Self::conformance_owner(&entry)),
                enclosing_bounds: entry.declared_bounds.clone(),
            });
        }
        Ok(templates)
    }

    /// The item query a resolved aggregate type came from, which is also the
    /// context its own declarations are analyzed in.
    fn owner_item_key(&mut self, owner: &ResolvedType) -> Option<(ItemKey, ResolvedType)> {
        let key = match owner {
            ResolvedType::Struct(cell) => {
                let owner = cell.borrow();
                ItemKey::new(
                    &owner.module_path,
                    &owner.name,
                    self.local_item_index(&owner.module_path, &owner.name)
                        .ok()?,
                    &owner.generic_args,
                )
            }
            ResolvedType::Union(cell) => {
                let owner = cell.borrow();
                ItemKey::new(
                    &owner.module_path,
                    &owner.name,
                    self.local_item_index(&owner.module_path, &owner.name)
                        .ok()?,
                    &owner.generic_args,
                )
            }
            ResolvedType::Enum { cell, .. } => {
                let owner = cell.borrow();
                ItemKey::new(
                    &owner.module_path,
                    &owner.name,
                    self.local_item_index(&owner.module_path, &owner.name)
                        .ok()?,
                    &owner.generic_args,
                )
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
