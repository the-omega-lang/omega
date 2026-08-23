//! Structural expansion of type-form `alias` declarations.
//!
//! Expansion happens on written `Type` syntax rather than on a resolved type,
//! because the meaning of an alias target depends on where it is used.
//! `spec A + B` is a set of bounds in a generic-parameter list, an anonymous
//! bounded generic in a parameter list, and a dynamic object behind a pointer.
//! Turning the alias into a `ResolvedType` first would erase that distinction
//! and force each of those positions to learn a second, alias-only format.

use crate::resolver::{ImportTarget, ItemAccess, ModuleResolver, ResolveError, ResolvedAlias};
use omega_parser::prelude::{FunctionType, Ident, Param, Type};

/// Replaces alias-owned generic parameter names with the types written for
/// them at the use site. Substituted types keep their own paths, and with them
/// their own resolution module, which is what lets an alias template resolve
/// its body at the declaration site and its arguments at the use site.
pub fn substitute_type_params(ty: &Type, subst: &[(Ident, Type)]) -> Type {
    let recur = |t: &Type| substitute_type_params(t, subst);
    match ty {
        Type::Named(path) if path.is_unqualified() => subst
            .iter()
            .find(|(name, _)| name == &path.head)
            .map(|(_, replacement)| replacement.clone())
            .unwrap_or_else(|| ty.clone()),
        Type::Named(_) => ty.clone(),
        Type::Pointer(inner, mutable) => Type::Pointer(Box::new(recur(inner)), *mutable),
        Type::InferredArray(inner) => Type::InferredArray(Box::new(recur(inner))),
        Type::UnknownSizeArray(inner) => Type::UnknownSizeArray(Box::new(recur(inner))),
        Type::SizedArray(inner, size) => Type::SizedArray(Box::new(recur(inner)), size.clone()),
        Type::SpecStatic(members) => Type::SpecStatic(members.iter().map(recur).collect()),
        Type::AnonymousEnum(members) => Type::AnonymousEnum(members.iter().map(recur).collect()),
        Type::Generic(path, args) => Type::Generic(path.clone(), args.iter().map(recur).collect()),
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

/// The alias a written type names, if any: where it is declared, the
/// arguments the use site supplied, and the authorization the binding it was
/// reached through already established. The accessor is the module the type
/// was written in, which is the module the alias's own visibility is judged
/// against.
struct AliasReference {
    accessor: Vec<Ident>,
    module: Vec<Ident>,
    name: Ident,
    args: Vec<Type>,
    bypass_visibility: bool,
}

fn alias_reference(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    ty: &Type,
) -> Result<Option<AliasReference>, ResolveError> {
    let (path, args) = match ty {
        Type::Named(path) => (path, Vec::new()),
        Type::Generic(path, args) => (path, args.clone()),
        _ => return Ok(None),
    };
    let accessor = resolver
        .macro_origin_module(path.origin)
        .unwrap_or_else(|| module_path.to_vec());
    let access = if let Some(anchored) = resolver.resolve_explicit_anchor(&accessor, path) {
        ItemAccess::gated(anchored?)
    } else if path.tail.is_empty() {
        // An import binds the same name a local declaration would, so a bare
        // reference to an imported structural alias must resolve at the
        // alias's own declaration module, not `accessor` itself.
        match resolver.resolve_import_alias(&accessor, &path.head)? {
            Some(ImportTarget::ItemPath(access)) => access,
            _ => ItemAccess::gated(
                accessor
                    .iter()
                    .cloned()
                    .chain(std::iter::once(path.head.clone()))
                    .collect(),
            ),
        }
    } else {
        let Some(ImportTarget::Module(base)) =
            resolver.resolve_import_alias(&accessor, &path.head)?
        else {
            return Ok(None);
        };
        ItemAccess::gated(base.into_iter().chain(path.tail.iter().cloned()).collect())
    };
    let mut access = access;
    if let Some((name, module)) = access.absolute.split_last() {
        let name = name.clone();
        let module = module.to_vec();
        if let Some(canonical_module) = resolver.resolve_module_path(&accessor, &module)? {
            access.absolute = canonical_module
                .into_iter()
                .chain(std::iter::once(name))
                .collect();
        }
    }
    let (name, module) = access
        .absolute
        .split_last()
        .expect("an alias reference path is never empty");
    Ok(Some(AliasReference {
        accessor,
        module: module.to_vec(),
        name: name.clone(),
        args,
        bypass_visibility: access.bypass_visibility,
    }))
}

/// One alias template applied to one written reference: the alias's
/// right-hand side with every parameter bound (explicit argument or default)
/// and substituted. `Ok(None)` means the reference does not name a
/// structural alias -- which includes naming an ordinary declaration through
/// a plain-path alias, since that is forwarding rather than expansion, and
/// is handled by ordinary item resolution.
///
/// Every argument is normalized *before* it is substituted, and the template
/// body is normalized *before* the arguments are substituted into it. That
/// ordering is what makes each obligation appear exactly once: an argument
/// used twice in a body (`alias Duo<T> = Pair<T, T>`) is checked once, while
/// an alias written in the body (`alias Outer<T> = Pair<Inner<T>, T>`) still
/// contributes `Inner`'s own obligation against the real argument.
///
/// `placeholders` are the enclosing template's own parameters, which are
/// opaque here: they name nothing resolvable until they are substituted.
fn apply_alias_once(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    placeholders: &[Ident],
    ty: &Type,
    obligations: &mut Vec<(Type, Type)>,
) -> Result<Option<Type>, ResolveError> {
    if let Type::Named(path) | Type::Generic(path, _) = ty
        && path.is_unqualified()
        && placeholders.contains(&path.head)
    {
        return Ok(None);
    }
    let Some(reference) = alias_reference(resolver, module_path, ty)? else {
        return Ok(None);
    };
    let Some(ResolvedAlias::Type { generics, r#type }) = resolver.resolve_visible_alias(
        &reference.accessor,
        &reference.module,
        &reference.name,
        reference.bypass_visibility,
    )?
    else {
        return Ok(None);
    };
    let arity_mismatch = || ResolveError::GenericArgCountMismatch {
        module: reference.module.clone(),
        item: reference.name.clone(),
        expected: generics.len(),
        found: reference.args.len(),
    };
    if reference.args.len() > generics.len() {
        return Err(arity_mismatch());
    }

    let mut subst: Vec<(Ident, Type)> = Vec::with_capacity(generics.len());
    for (index, param) in generics.iter().enumerate() {
        let written = match (reference.args.get(index), &param.default) {
            (Some(argument), _) => argument.clone(),
            (None, Some(default)) => substitute_type_params(default, &subst),
            (None, None) => return Err(arity_mismatch()),
        };
        let argument = normalize_type(resolver, module_path, placeholders, &written, obligations)?;
        for bound in &param.bounds {
            // A bound sees the parameters declared before it, exactly as a
            // default does. It stays unexpanded in the obligation: a bound
            // naming `spec A + B` is a bound *list*, and flattening it is the
            // bound checker's own job.
            let bound = substitute_type_params(bound, &subst);
            normalize_type(resolver, module_path, placeholders, &bound, obligations)?;
            obligations.push((bound, argument.clone()));
        }
        subst.push((param.ident.clone(), argument));
    }

    let own: Vec<Ident> = generics.iter().map(|g| g.ident.clone()).collect();
    let mut body_obligations = Vec::new();
    let body = normalize_type(resolver, module_path, &own, &r#type, &mut body_obligations)?;
    obligations.extend(body_obligations.into_iter().map(|(bound, argument)| {
        (
            substitute_type_params(&bound, &subst),
            substitute_type_params(&argument, &subst),
        )
    }));
    Ok(Some(substitute_type_params(&body, &subst)))
}

/// Normalizes a whole written type: every alias application anywhere in it
/// is expanded, and every alias-owned obligation the applications create is
/// appended to `obligations`. Aliases reached only through another alias's
/// right-hand side, default, or bound are reached here too, which a
/// root-only expansion loop could not do.
fn normalize_type(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    placeholders: &[Ident],
    ty: &Type,
    obligations: &mut Vec<(Type, Type)>,
) -> Result<Type, ResolveError> {
    // An expansion is already normalized: its body and arguments were
    // normalized before they were joined, so re-walking it would report
    // every obligation inside it a second time.
    if let Some(expanded) = apply_alias_once(resolver, module_path, placeholders, ty, obligations)?
    {
        return Ok(expanded);
    }
    let mut recur = |inner: &Type, obligations: &mut Vec<(Type, Type)>| {
        normalize_type(resolver, module_path, placeholders, inner, obligations)
    };
    Ok(match ty {
        Type::Named(_) => ty.clone(),
        Type::Pointer(inner, mutable) => {
            Type::Pointer(Box::new(recur(inner, obligations)?), *mutable)
        }
        Type::InferredArray(inner) => Type::InferredArray(Box::new(recur(inner, obligations)?)),
        Type::UnknownSizeArray(inner) => {
            Type::UnknownSizeArray(Box::new(recur(inner, obligations)?))
        }
        Type::SizedArray(inner, size) => {
            Type::SizedArray(Box::new(recur(inner, obligations)?), size.clone())
        }
        Type::Generic(path, args) => {
            let mut normalized = Vec::with_capacity(args.len());
            for arg in args {
                normalized.push(recur(arg, obligations)?);
            }
            Type::Generic(path.clone(), normalized)
        }
        Type::SpecStatic(members) => {
            let mut normalized = Vec::with_capacity(members.len());
            for member in members {
                normalized.push(recur(member, obligations)?);
            }
            Type::SpecStatic(normalized)
        }
        Type::AnonymousEnum(members) => {
            let mut normalized = Vec::with_capacity(members.len());
            for member in members {
                normalized.push(recur(member, obligations)?);
            }
            Type::AnonymousEnum(normalized)
        }
        Type::Function(f) => {
            let mut params = Vec::with_capacity(f.params.len());
            for param in &f.params {
                params.push(Param {
                    r#type: recur(&param.r#type, obligations)?,
                    ..param.clone()
                });
            }
            Type::Function(FunctionType {
                params,
                return_type: Box::new(recur(&f.return_type, obligations)?),
                is_variadic: f.is_variadic,
                self_mode: f.self_mode,
                convention: f.convention.clone(),
            })
        }
    })
}

/// Expands `ty` through every alias application in it, discarding the
/// obligations: the caller either does not need them (plain resolution) or
/// collects them separately via [`applied_alias_bounds`].
pub fn expand_type_alias(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    ty: Type,
) -> Result<Type, ResolveError> {
    normalize_type(resolver, module_path, &[], &ty, &mut Vec::new())
}

/// Flattens a written bound list. A bound naming an alias of `spec A + B`
/// contributes each member, so `<T: AB>` and `<T: A + B>` produce the same
/// bound set rather than making bound checking learn an alias-only format.
pub fn expand_bounds(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    bounds: &[Type],
) -> Result<Vec<Type>, ResolveError> {
    let mut expanded = Vec::with_capacity(bounds.len());
    for bound in bounds {
        match expand_type_alias(resolver, module_path, bound.clone())? {
            Type::SpecStatic(members) => expanded.extend(members),
            _ => expanded.push(bound.clone()),
        }
    }
    Ok(expanded)
}

/// Every alias-owned generic bound a written type applies, anywhere in it,
/// paired with the (possibly defaulted) argument bound to each. The caller
/// resolves and checks them; normalization itself stays syntactic.
pub fn applied_alias_bounds(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    ty: &Type,
) -> Result<Vec<(Type, Type)>, ResolveError> {
    let mut obligations = Vec::new();
    normalize_type(resolver, module_path, &[], ty, &mut obligations)?;
    Ok(obligations)
}
