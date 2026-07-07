use crate::resolved_type::ResolvedType;
use omega_parser::prelude::{Ident, Type};
use std::collections::HashMap;

/// Structurally unifies `raw` (a generic function template's own declared
/// parameter type, exactly as written in source, still referencing its
/// generic parameter names) against `concrete` (a call's already-resolved
/// argument type) to deduce a binding for any of `generics` found at a
/// `Type::Named` leaf -- the duck-typed, argument-driven inference behind
/// `Analyzer::resolve_generic_call`.
///
/// The first binding found for a given generic name wins; a later,
/// differently-typed occurrence of the same name isn't treated as an error
/// here -- "duck typed" means unification's only job is a best-effort
/// deduction, not full verification. Any real mismatch (including a raw
/// shape that doesn't structurally match `concrete` at all) is simply left
/// unbound/unresolved and caught afterward by the ordinary, unchanged
/// argument-type-matching loop, once the concrete instantiated signature
/// actually exists.
///
/// Recurses through `Pointer`/`Array`/`SizedArray`/`Function` to find a
/// generic parameter nested inside a compound shape (e.g. a parameter
/// declared `item: *T`), including the same `*[T]` -> `Slice` special case
/// `Context::resolve_type` applies when *resolving* (rather than unifying)
/// a type.
pub fn unify_generic_type(
    generics: &[Ident],
    raw: &Type,
    concrete: &ResolvedType,
    subst: &mut HashMap<Ident, ResolvedType>,
) {
    match (raw, concrete) {
        (Type::Named(path), _) if path.is_unqualified() && generics.contains(&path.head) => {
            subst.entry(path.head.clone()).or_insert_with(|| concrete.clone());
        }
        // `*[T]` only ever resolves to `Slice`, never `Pointer` (see
        // `Context::resolve_type`'s identical special case) -- so a raw
        // `Pointer(Array(_))` shape only ever unifies against a `Slice`,
        // regardless of whether `concrete` actually turns out to be one (a
        // mismatch here is left for the ordinary argument-type check).
        (Type::Pointer(inner), _) if matches!(inner.as_ref(), Type::Array(_)) => {
            let Type::Array(elem) = inner.as_ref() else { unreachable!() };
            if let ResolvedType::Slice(c) = concrete {
                unify_generic_type(generics, elem, c, subst);
            }
        }
        (Type::Pointer(inner), ResolvedType::Pointer(c)) => unify_generic_type(generics, inner, c, subst),
        (Type::Array(inner), ResolvedType::Array(c)) => unify_generic_type(generics, inner, c, subst),
        (Type::SizedArray(inner, _), ResolvedType::SizedArray(c, _)) => unify_generic_type(generics, inner, c, subst),
        (Type::Function(f), ResolvedType::Function(c)) => {
            for ((_, p), (_, cp)) in f.params.iter().zip(&c.params) {
                unify_generic_type(generics, p, cp, subst);
            }
            unify_generic_type(generics, &f.return_type, &c.return_type, subst);
        }
        _ => {}
    }
}
