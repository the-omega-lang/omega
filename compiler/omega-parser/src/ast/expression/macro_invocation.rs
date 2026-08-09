use crate::ast::identifier::Ident;
use crate::lexer::Token;

/// `name$(arg, ...)` -- a macro invocation. Shared verbatim between all
/// three invocation positions -- expression (`Expression::MacroInvocation`,
/// usable anywhere an expression can appear), whole-statement
/// (`Statement::MacroInvocation`), and module-top-level item
/// (`Item::MacroInvocation`) -- rather than duplicated into three
/// near-identical types, since the grammar and payload shape are identical
/// in every case; only *where* the parser is wired in, and therefore which
/// grammar the expansion is re-parsed with, differs. Repetition is a
/// property of the *definition's* body, never of an argument list, so
/// `args` stays a flat list of raw token runs regardless of whether the
/// callee declares a variadic parameter. Each argument is kept as a raw
/// token slice (not parsed
/// as an `Expression`/`Type` here) since a `Type`-fragment argument (e.g.
/// `generate_type$(Counter)`) isn't valid expression syntax; see
/// `omega_parser::macros` for where each argument is validated against its
/// parameter's declared `FragmentKind` and substituted.
#[derive(Debug, Clone)]
pub struct MacroInvocationExpr {
    pub name: Ident,
    pub args: Vec<Vec<Token>>,
}
