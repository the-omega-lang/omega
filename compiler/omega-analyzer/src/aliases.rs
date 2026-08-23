//! Structural expansion of type-form `alias` declarations.
//!
//! Expansion happens on written `Type` syntax rather than on a resolved type,
//! because the meaning of an alias target depends on where it is used.
//! `spec A + B` is a set of bounds in a generic-parameter list, an anonymous
//! bounded generic in a parameter list, and a dynamic object behind a pointer.
//! Turning the alias into a `ResolvedType` first would erase that distinction
//! and force each of those positions to learn a second, alias-only format.

use crate::resolver::{ImportTarget, ModuleResolver, ResolveError, ResolvedAlias};
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

/// The alias a written type names, if any, together with the arguments the use
/// site supplied for it.
struct AliasReference {
    module: Vec<Ident>,
    name: Ident,
    args: Vec<Type>,
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
    let resolution_module = resolver
        .macro_origin_module(path.origin)
        .unwrap_or_else(|| module_path.to_vec());
    let (module, name) = if path.is_unqualified() {
        (resolution_module, path.head.clone())
    } else {
        let Some(ImportTarget::Module(base)) =
            resolver.resolve_import_alias(&resolution_module, &path.head)?
        else {
            return Ok(None);
        };
        let mut absolute = base;
        absolute.extend(path.tail.iter().cloned());
        let (name, module) = absolute
            .split_last()
            .expect("a qualified path has at least two segments");
        (module.to_vec(), name.clone())
    };
    Ok(Some(AliasReference { module, name, args }))
}

/// One layer of type-form alias expansion. `Ok(None)` means `ty` does not name
/// a structural alias, which includes naming an ordinary declaration through a
/// plain-path alias -- that case is forwarding, not expansion, and is handled
/// by ordinary item resolution.
pub fn expand_type_alias_once(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    ty: &Type,
) -> Result<Option<Type>, ResolveError> {
    let Some(reference) = alias_reference(resolver, module_path, ty)? else {
        return Ok(None);
    };
    let Some(ResolvedAlias::Type { generics, r#type }) =
        resolver.resolve_declared_alias(&reference.module, &reference.name)?
    else {
        return Ok(None);
    };
    if reference.args.len() > generics.len() {
        return Err(ResolveError::GenericArgCountMismatch {
            module: reference.module,
            item: reference.name,
            expected: generics.len(),
            found: reference.args.len(),
        });
    }
    let mut subst: Vec<(Ident, Type)> = Vec::with_capacity(generics.len());
    for (index, param) in generics.iter().enumerate() {
        let argument = match (reference.args.get(index), &param.default) {
            (Some(argument), _) => argument.clone(),
            (None, Some(default)) => substitute_type_params(default, &subst),
            (None, None) => {
                return Err(ResolveError::GenericArgCountMismatch {
                    module: reference.module,
                    item: reference.name,
                    expected: generics.len(),
                    found: reference.args.len(),
                });
            }
        };
        subst.push((param.ident.clone(), argument));
    }
    Ok(Some(substitute_type_params(&r#type, &subst)))
}

/// Repeats [`expand_type_alias_once`] until the type no longer names a
/// structural alias. Cycles are impossible here: the alias query itself
/// reports a cycle before returning a target that could close the loop.
pub fn expand_type_alias(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    ty: Type,
) -> Result<Type, ResolveError> {
    let mut current = ty;
    while let Some(expanded) = expand_type_alias_once(resolver, module_path, &current)? {
        current = expanded;
    }
    Ok(current)
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

/// The alias-owned generic bounds a written type applies, paired with the
/// argument written for each. The caller resolves and checks them; expansion
/// itself stays syntactic.
pub fn applied_alias_bounds(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    ty: &Type,
) -> Result<Vec<(Type, Type)>, ResolveError> {
    let Some(reference) = alias_reference(resolver, module_path, ty)? else {
        return Ok(vec![]);
    };
    let Some(ResolvedAlias::Type { generics, .. }) =
        resolver.resolve_declared_alias(&reference.module, &reference.name)?
    else {
        return Ok(vec![]);
    };
    let mut pairs = Vec::new();
    for (param, argument) in generics.iter().zip(&reference.args) {
        for bound in &param.bounds {
            pairs.push((bound.clone(), argument.clone()));
        }
    }
    Ok(pairs)
}
