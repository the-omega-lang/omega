use crate::ast::r#type::Type;

/// `sizeof<Type>` -- a compile-time size query, evaluating to the target's
/// `usize`. Unlike `CastExpr`, this has no `base` expression at all: it's a
/// pure function of a type, not an operator applied to a value. Parsed the
/// same way any other generic-looking construct in this grammar is (see
/// `parser::expression::parse_sizeof`); `sizeof` itself is a contextual
/// keyword (like `self`/`mut`), recognized only when immediately followed
/// by `<` -- an ordinary variable named `sizeof` used any other way still
/// parses as a plain identifier.
#[derive(Debug, Clone)]
pub struct SizeofExpr {
    pub r#type: Type,
}
