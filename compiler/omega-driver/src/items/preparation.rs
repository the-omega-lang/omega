use super::*;
use omega_analyzer::analysis::AnalysisSite;
use omega_analyzer::generics::GenericSubstitution;
use omega_analyzer::resolved_type::{ResolvedGenericArg, ResolvedSpecType};
use omega_analyzer::resolver::{DeclaredBound, GenericCallTarget, PreparedCall, UnmetBound};
use std::cell::RefCell;
use std::rc::Rc;

/// One generic function declaration, together with everything the context it
/// was reached through already binds for it.
pub(crate) struct GenericDeclaration {
    pub key: ItemKey,
    pub function: HirFunctionDef,
    pub site: AnalysisSite,
    /// The owner's generic arguments and `Self`, empty for a free function.
    pub enclosing: GenericSubstitution,
}

/// A declared bound, paired with the spec it names so that proving it needs
/// no second lookup.
type Bound = (Rc<RefCell<ResolvedSpecType>>, DeclaredBound);

impl Driver {
    pub(crate) fn prepare_generic_call(
        &mut self,
        target: GenericCallTarget<'_>,
        arguments: &[Option<ResolvedGenericArg>],
    ) -> Result<Option<PreparedCall>, ResolveError> {
        let Some(declaration) = self.generic_declaration(target)? else {
            return Ok(None);
        };
        let GenericDeclaration {
            key,
            function,
            site,
            enclosing,
        } = declaration;
        let arguments =
            self.complete_overload_arguments(&key, &function, &enclosing, arguments)?;
        let bounds =
            self.declared_bounds_under(&key, site, &enclosing, &function.generics, &arguments)?;
        let unmet = self.first_unmet_bound(&function.generics, &arguments, &bounds);
        Ok(Some(PreparedCall {
            arguments,
            bounds: bounds
                .into_iter()
                .map(|set| set.into_iter().map(|(_, bound)| bound).collect())
                .collect(),
            unmet,
        }))
    }

    /// The declaration each of the three ways a call site names a generic
    /// function reaches. `Ok(None)` means the name is not one.
    fn generic_declaration(
        &mut self,
        target: GenericCallTarget<'_>,
    ) -> Result<Option<GenericDeclaration>, ResolveError> {
        match target {
            GenericCallTarget::Declaration(declaration) => {
                let Some(key) = self.items.decl_id_owner.get(&declaration).cloned() else {
                    return Ok(None);
                };
                if key.owner().is_some() {
                    return Ok(Some(self.method_declaration_for_key(&key)));
                }
                let hir = self.modules.hir(key.module());
                let HirItem::FunctionDefinition(function) = &hir.items[key.disambiguator] else {
                    return Ok(None);
                };
                let function = function.clone();
                let function = self.normalized_function(key.module(), &function)?;
                let site = AnalysisSite::new(function.id, function.span);
                Ok(Some(GenericDeclaration {
                    key,
                    function,
                    site,
                    enclosing: GenericSubstitution::new(),
                }))
            }
            GenericCallTarget::Function(absolute) => {
                let absolute = self.canonical_query_path(absolute);
                let Some((name, module)) = absolute.split_last() else {
                    return Ok(None);
                };
                let Ok(index) = self.local_item_index(module, name) else {
                    return Ok(None);
                };
                let HirItem::FunctionDefinition(function) =
                    &self.modules.parsed(module).hir.items[index]
                else {
                    return Ok(None);
                };
                let function = function.clone();
                let function = self.normalized_function(module, &function)?;
                let site = AnalysisSite::new(function.id, function.span);
                Ok(Some(GenericDeclaration {
                    key: ItemKey {
                        scope: ItemScope::Module(module.to_vec()),
                        disambiguator: index,
                        name: name.clone(),
                        generic_args: Vec::new(),
                    },
                    function,
                    site,
                    enclosing: GenericSubstitution::new(),
                }))
            }
            GenericCallTarget::Method {
                owner,
                name,
                namespace,
            } => self.generic_method_declaration(owner, name, namespace),
        }
    }

    /// The bounds each generic parameter declares once `arguments` are
    /// substituted, resolved in the declaration's own module under the owner
    /// instantiation it was reached through. Conformance is not consulted:
    /// this is what the declaration says, not what the arguments prove.
    fn declared_bounds_under(
        &mut self,
        key: &ItemKey,
        site: AnalysisSite,
        enclosing: &GenericSubstitution,
        generic_params: &[HirGenericParam],
        generic_args: &[ResolvedGenericArg],
    ) -> Result<Vec<Vec<Bound>>, ResolveError> {
        let module = key.module().clone();
        let mut substitution: GenericSubstitution = generic_params
            .iter()
            .map(|g| g.ident.clone())
            .zip(generic_args.iter().cloned())
            .collect();
        for (name, arg) in enclosing.iter() {
            substitution.push(name.clone(), arg.clone());
        }

        let mut declared = Vec::with_capacity(generic_params.len());
        for param in generic_params {
            let expanded =
                omega_analyzer::aliases::expand_bounds(self, &module, param.bounds())?;
            let mut set: Vec<Bound> = Vec::new();
            for bound in &expanded {
                let run = self.with_analyzer(&module, &substitution, site, |analyzer| {
                    analyzer.bound_key(site.id, site.span, bound)
                });
                let Some(bound) = run.result else {
                    return Err(key.failed());
                };
                if !set.iter().any(|(_, existing)| *existing == bound.1) {
                    set.push(bound);
                }
            }
            declared.push(set);
        }
        Ok(declared)
    }

    /// The first declared bound these arguments do not prove. A bound is
    /// proved by an actual conformance witness, never by a type that happens
    /// to declare matching methods.
    fn first_unmet_bound(
        &mut self,
        generic_params: &[HirGenericParam],
        generic_args: &[ResolvedGenericArg],
        declared: &[Vec<Bound>],
    ) -> Option<UnmetBound> {
        for ((param, arg), set) in generic_params.iter().zip(generic_args).zip(declared) {
            // A `comp` parameter binds a value and declares no bounds, so it
            // is skipped rather than ending the scan over the later ones.
            let Some(concrete) = arg.as_type().cloned() else {
                continue;
            };
            for (spec, bound) in set {
                if self
                    .conformance_for(&concrete, spec, &bound.args)
                    .is_none()
                {
                    return Some(UnmetBound {
                        parameter: param.ident.clone(),
                        r#type: concrete,
                        spec: bound.name.clone(),
                    });
                }
            }
        }
        None
    }
}
