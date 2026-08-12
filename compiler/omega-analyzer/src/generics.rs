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
/// Recurses through `Pointer`/`SizedArray`/`Function`/`Generic` to find a
/// generic parameter nested inside a compound shape (e.g. a parameter
/// declared `item: *T`, or `item: Pair<T>`), including the same
/// `*[]T` -> `Array` / `*[?]T` -> `Slice` dedicated productions
/// `Context::resolve_pointer_type` applies when *resolving* (rather than
/// unifying) a type.
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
        // `*[]T`/`*[?]T` only ever resolve to `Array`/`Slice`, never a
        // plain `Pointer` (see `Context::resolve_pointer_type`) -- so these
        // raw shapes only ever unify against the matching `ResolvedType`,
        // regardless of whether `concrete` actually turns out to be one (a
        // mismatch here is left for the ordinary argument-type check).
        (Type::Pointer(inner, _), ResolvedType::Array(c, _))
            if matches!(inner.as_ref(), Type::UnsizedArray(_)) =>
        {
            let Type::UnsizedArray(elem) = inner.as_ref() else {
                unreachable!()
            };
            unify_generic_type(generics, elem, c, subst);
        }
        (Type::Pointer(inner, _), ResolvedType::Slice { item: c, .. })
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
            for ((_, p), (_, cp)) in f.params.iter().zip(&c.params) {
                unify_generic_type(generics, p, cp, subst);
            }
            unify_generic_type(generics, &f.return_type, &c.return_type, subst);
        }
        // `Pair<T>` (a parameter typed as a still-generic struct/enum/union)
        // against `Pair<i32>` (the call's own already-resolved argument) --
        // zips `raw`'s own written arguments positionally against the
        // concrete owner's own `type_args` (populated positionally against
        // the declaration's generic parameter list when that cell was
        // built, see `ResolvedStructType`/`ResolvedEnumType`/
        // `ResolvedUnionType::type_args`) and recurses into each pair, the
        // same way a nested `Pointer`/`Array` shape already does. No check
        // that `raw`'s own path name actually names the same owner as
        // `concrete` -- matches this function's own "duck typed, best-effort,
        // any real mismatch is caught afterward by the ordinary argument
        // check" contract (see this function's doc comment): a wrong guess
        // here can never be silently accepted, only ever left unbound or
        // corrected by that later check.
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

/// Turns a completed `unify_generic_type` substitution into `generics`'
/// own ordered `Vec<ResolvedType>`, or the first generic name that never
/// got a binding and has no declared default either -- the shared tail end
/// of every duck-typed inference site (a generic function call, a generic
/// struct/enum/union literal, ...): each has its own reason to know *which*
/// generic came up empty (to shape its own diagnostic), so this stops at
/// the first real miss and hands the name back rather than picking a
/// wording itself.
///
/// An unbound generic that *does* have a declared default is not resolved
/// here -- it's left for the returned vec to simply be shorter than
/// `generics`, trusting the caller to hand it on to `ensure_item`'s own
/// default-padding gate (see `omega_driver::items::ensure_item`), the one
/// place a default `Type` is actually turned into a `ResolvedType` for
/// this "no argument ever bound it" case. This is only ever safe because
/// defaults are enforced trailing-only at parse time
/// (`omega_parser`'s `DefaultGenericParamNotTrailing`): the first unbound
/// generic with a default means every generic after it has one too, so
/// stopping here can never strand a later, still-explicit generic behind
/// an unfilled gap.
///
/// Every deduced type is widened -- a deduced `T` must never carry a
/// caller-specific enum-variant refinement (`T = MyEnum`, not `T =
/// MyEnum::Second`), which would mint a spurious extra instantiation per
/// variant.
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

/// `concrete`'s own generic type arguments, if it's a struct/enum/union
/// instantiation -- `None` for anything else (including a non-generic
/// struct/enum/union, whose `type_args` is simply empty, and every other
/// `ResolvedType` shape, which has no such field at all). The one piece of
/// data `unify_generic_type`'s `Type::Generic` arm needs to recurse into a
/// generic owner's own arguments.
fn owner_type_args(concrete: &ResolvedType) -> Option<Vec<ResolvedType>> {
    match concrete {
        ResolvedType::Struct(cell) => Some(cell.borrow().type_args.clone()),
        ResolvedType::Enum { cell, .. } => Some(cell.borrow().type_args.clone()),
        ResolvedType::Union(cell) => Some(cell.borrow().type_args.clone()),
        _ => None,
    }
}
