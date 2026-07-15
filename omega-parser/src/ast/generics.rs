use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

/// One `<...>` entry on a generic-bearing item (function, struct, union,
/// enum, or spec): a name, plus an optional single spec bound (`T: Animal`).
/// `bound: None` is an ordinary duck-typed generic, resolved purely
/// structurally, exactly as generics behaved before specs existed. A bound
/// generic must nominally implement that spec (`struct Dog : Animal`) --
/// structural satisfaction alone never counts. Only one bound is ever
/// parsed here (see `SpecStmt`'s doc comment for why): a function needing
/// several unrelated specs at once names an alias spec instead of stacking
/// bounds.
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub ident: Ident,
    pub bound: Option<Type>,
}
