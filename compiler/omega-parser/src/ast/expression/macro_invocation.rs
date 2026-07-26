use crate::ast::identifier::Ident;
use crate::lexer::Token;

/// `name!(arg, ...)` -- a macro invocation. Shared verbatim between
/// expression position (`Expression::MacroInvocation`, usable anywhere an
/// expression can appear) and module-top-level item position
/// (`Item::MacroInvocation`, for an `items`-output macro) rather
/// than duplicated into two near-identical types, since the grammar and
/// payload shape are identical either way -- only *where* the parser is
/// wired in differs. Each argument is kept as a raw token slice (not parsed
/// as an `Expression`/`Type` here) since a `Type`-fragment argument (e.g.
/// `generate_type!(Counter)`) isn't valid expression syntax; see
/// `omega_parser::macros` for where each argument is validated against its
/// parameter's declared `FragmentKind` and substituted.
#[derive(Debug, Clone)]
pub struct MacroInvocationExpr {
    pub name: Ident,
    pub args: Vec<Vec<Token>>,
}
