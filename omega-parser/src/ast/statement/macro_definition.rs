use crate::ast::identifier::Ident;
use crate::lexer::Token;

/// What grammar a macro parameter's captured argument must parse as.
/// Deliberately small (just the two forms the language needs today) rather
/// than open-ended -- adding another (e.g. `ident`, `stmt`) is a new
/// `FragmentKind` variant plus one new arm wherever a fragment kind is
/// validated/re-parsed (`omega_parser::macros::expand_invocation`), not an
/// architectural change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentKind {
    Expr,
    Type,
}

/// What a macro invocation expands into, and therefore which grammar
/// position it's usable in: `Expr` -- an `Expression::MacroInvocation`,
/// usable anywhere an expression can appear; `Items` -- a
/// `RootStatement::MacroInvocation`, usable only at module top level,
/// expanding to zero or more top-level items (structs, functions, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroOutputKind {
    Expr,
    Items,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: Ident,
    pub kind: FragmentKind,
}

/// `macro name($a: expr, $b: type, ...) => expr|items { ... }` -- the body
/// is captured as a raw token slice, *not* run through the
/// `Expression`/`Statement`/`RootStatement` parsers here: it legitimately
/// contains `$name` metavariables (not valid identifiers on their own) and,
/// for an `Items`-output macro, syntax that only becomes valid once `$name`
/// is substituted with a concrete identifier (e.g. `struct $name { ... }`).
/// See `omega_parser::macros` for how a definition's body is later
/// substituted and re-parsed for real at each invocation site.
#[derive(Debug, Clone)]
pub struct MacroDefStmt {
    pub name: Ident,
    pub params: Vec<MacroParam>,
    pub output: MacroOutputKind,
    pub body: Vec<Token>,
}
