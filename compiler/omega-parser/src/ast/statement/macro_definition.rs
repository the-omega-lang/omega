use crate::ast::identifier::Ident;
use crate::ast::visibility::Visibility;
use crate::diagnostics::Span;
use crate::lexer::Token;
/// What grammar a macro parameter's captured argument must parse as.
/// Deliberately small (just the forms the language needs today) rather
/// than open-ended -- adding another (e.g. `stmt`) is a new
/// `FragmentKind` variant plus one new arm wherever a fragment kind is
/// validated/re-parsed (`omega_parser::macros::validate_fragment`), not an
/// architectural change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentKind {
    Expr,
    Type,
    Ident,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: Ident,
    pub kind: FragmentKind,
}

/// Fixed parameters plus the optional, necessarily final variadic parameter.
#[derive(Debug, Clone)]
pub struct MacroSignature {
    pub fixed: Vec<MacroParam>,
    pub variadic: Option<MacroParam>,
}

/// One piece of a macro body. A body is a *tree* rather than a flat token
/// list purely because repetition nests; ordinary bracketed groups do not
/// (`(`/`)`/... stay individual `Token` pieces, exactly as the lexer
/// produces them).
#[derive(Debug, Clone)]
pub enum MacroBodyPiece {
    /// Any ordinary token, including a `$name` metavariable.
    Token(Token),
    Repetition(MacroRepetition),
}

/// `$...( sep? ) { body }` -- expands `body` once per variadic argument.
#[derive(Debug, Clone)]
pub struct MacroRepetition {
    /// Emitted between consecutive expansions, never before the first or
    /// after the last. `None` for `$...(){ ... }`.
    pub separator: Option<Token>,
    pub body: Vec<MacroBodyPiece>,
    pub span: Span,
}

/// `macro name($a: expr, $b: type...) => { ... }` -- the body is not run
/// through the `Expression`/`Statement`/`Item` parsers here: it legitimately
/// contains `$name` metavariables (not valid identifiers on their own) and
/// syntax that only becomes valid once `$name` is substituted with a
/// concrete identifier (e.g. `struct $name { ... }`). There is no declared
/// output kind -- which grammar an expansion is parsed with is decided
/// entirely by the *invocation's* grammatical position (item, statement, or
/// expression). See `omega_parser::macros` for how a definition's body is
/// later substituted and re-parsed for real at each invocation site.
/// A macro definition. Its visibility follows the ordinary three-level item
/// rule: hidden stays file-local, `internal` reaches the package, and
/// `exposed` reaches all importers and the ambient `core` prelude.
#[derive(Debug, Clone)]
pub struct MacroDefinitionStmt {
    pub visibility: Visibility,
    pub name: Ident,
    pub signature: MacroSignature,
    pub body: Vec<MacroBodyPiece>,
    /// Filled in by the driver while collecting a module's macro environment.
    pub defining_module: Vec<Ident>,
}
