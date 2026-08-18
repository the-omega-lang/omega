use crate::resolved_type::ResolvedType;
use omega_parser::prelude::{Ident, Type};
use std::collections::HashMap;

pub fn unify_generic_type(
    generics: &[Ident],
    raw: &Type,
    concrete: &ResolvedType,
    subst: &mut HashMap<Ident, ResolvedType>,
) {
    match (raw, concrete) {
        (Type::Named(path), _) if path.is_unqualified() && generics.contains(&path.head) => {
            subst
                .entry(path.head.clone())
                .or_insert_with(|| concrete.clone());
        }
        (Type::Pointer(inner, _), ResolvedType::Slice { item: c, .. })
            if matches!(inner.as_ref(), Type::InferredArray(_)) =>
        {
            let Type::InferredArray(elem) = inner.as_ref() else {
                unreachable!()
            };
            unify_generic_type(generics, elem, c, subst);
        }
        (Type::Pointer(inner, _), ResolvedType::Array(c, _))
            if matches!(inner.as_ref(), Type::UnknownSizeArray(_)) =>
        {
            let Type::UnknownSizeArray(elem) = inner.as_ref() else {
                unreachable!()
            };
            unify_generic_type(generics, elem, c, subst);
        }
        (Type::Pointer(inner, _), ResolvedType::Pointer { pointee: c, .. }) => {
            unify_generic_type(generics, inner, c, subst)
        }
        (Type::SizedArray(inner, _), ResolvedType::SizedArray(c, _)) => {
            unify_generic_type(generics, inner, c, subst)
        }
        (Type::Function(f), ResolvedType::Function(c)) => {
            for (p, (_, cp)) in f.params.iter().zip(&c.params) {
                unify_generic_type(generics, &p.r#type, cp, subst);
            }
            unify_generic_type(generics, &f.return_type, &c.return_type, subst);
        }
        // `Pair<T>` against `Pair<i32>`: zips `raw`'s written arguments
        // positionally against the concrete owner's `type_args` and
        // recurses into each pair. No check that `raw`'s path actually
        // names the same owner as `concrete` -- a wrong guess here is
        // caught afterward by the ordinary argument check.
        (Type::Generic(_, raw_args), _) => {
            if let Some(concrete_args) = owner_type_args(concrete) {
                for (r, c) in raw_args.iter().zip(&concrete_args) {
                    unify_generic_type(generics, r, c, subst);
                }
            }
        }
        _ => {}
    }
}

pub fn resolve_inferred_type_args(
    generics: &[Ident],
    defaults: &[Option<Type>],
    subst: &HashMap<Ident, ResolvedType>,
) -> Result<Vec<ResolvedType>, Ident> {
    let mut type_args = Vec::with_capacity(generics.len());
    for (generic, default) in generics.iter().zip(defaults) {
        match subst.get(generic) {
            Some(resolved) => type_args.push(resolved.widened()),
            None if default.is_some() => break,
            None => return Err(generic.clone()),
        }
    }
    Ok(type_args)
}

fn owner_type_args(concrete: &ResolvedType) -> Option<Vec<ResolvedType>> {
    match concrete {
        ResolvedType::Struct(cell) => Some(cell.borrow().type_args.clone()),
        ResolvedType::Enum { cell, .. } => Some(cell.borrow().type_args.clone()),
        ResolvedType::Union(cell) => Some(cell.borrow().type_args.clone()),
        _ => None,
    }
}
