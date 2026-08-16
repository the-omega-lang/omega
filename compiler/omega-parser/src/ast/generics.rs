use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

/// One `<...>` entry on a generic-bearing item (function, struct, union,
/// enum, or spec): a name, plus zero or more spec bounds (`T: Animal +
/// Display`) and an optional default (`T = i32`). An empty `bounds` list is
/// an ordinary duck-typed generic, resolved purely structurally, exactly as
/// generics behaved before specs existed. A bound generic must nominally
/// implement every one of its specs (`conform Dog to Animal` and `conform
/// Dog to Display` both) -- structural satisfaction alone never counts.
/// `+` is the one separator: a conjunction names a *set* of requirements on
/// the same implementor, never a sum type.
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
    pub bounds: Vec<Type>,
    pub default: Option<Type>,
}
