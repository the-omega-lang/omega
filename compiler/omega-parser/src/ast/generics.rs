use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

/// One `<...>` entry on a generic-bearing item (function, struct, union,
/// enum, or spec): a name, plus an optional single spec bound (`T: Animal`)
/// and an optional default (`T = i32`). `bound: None` is an ordinary
/// duck-typed generic, resolved purely structurally, exactly as generics
/// behaved before specs existed. A bound generic must nominally implement
/// that spec (`struct Dog : Animal`) -- structural satisfaction alone never
/// counts. Only one bound is ever parsed here (see `SpecStmt`'s doc comment
/// for why): a function needing several unrelated specs at once names an
/// alias spec instead of stacking bounds.
///
/// `default` is the type used when a use site omits this parameter
/// entirely. It may reference any earlier parameter in the same list
/// (`struct Pair<A, B = A>`) but never a later one -- once one parameter in
/// a list has a default, every parameter after it must too (positional
/// generic arguments make this the only unambiguous omission shape),
/// enforced once the list is fully known (see
/// `omega_driver::modules::ModuleResolver::item_generics`).
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub ident: Ident,
    pub bound: Option<Type>,
    pub default: Option<Type>,
}
