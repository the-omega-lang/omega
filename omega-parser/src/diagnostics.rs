//! Parse-time error types, and their conversion into renderable
//! [`Diagnostic`]s. The position/rendering machinery itself
//! ([`Span`], `SourceFile`, `Renderer`) lives in `omega_diagnostics` -- this
//! module only owns what a *parser* knows: which grammar rule failed, and
//! what advice helps fix it.

use omega_diagnostics::Diagnostic;
pub use omega_diagnostics::Span;
use std::fmt;

/// One parse-time problem, anchored at the span it concerns. Recoverable:
/// `omega_parser`'s lexer/parser keep going after producing one of these
/// (see `parser::recovery`), collecting as many as it can into one
/// `Vec<ParseError>` rather than stopping at the first.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub kind: ParseErrorKind,
}

impl ParseError {
    pub fn new(span: Span, kind: ParseErrorKind) -> Self {
        Self { span, kind }
    }

    /// The renderable form of this error: same headline as `Display`, plus
    /// a caret label at the offending span and, where there's genuinely
    /// useful advice, a `help:`/`note:` footer. Advice is deliberately only
    /// attached where it's always true -- a wrong hint is worse than none.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let d = Diagnostic::error(self.kind.to_string());
        match &self.kind {
            ParseErrorKind::Expected { expected, .. } => d.with_label(self.span, format!("expected {expected}")),
            ParseErrorKind::UnterminatedString => d
                .with_label(self.span, "this string never closes")
                .with_help("add a closing `\"`"),
            ParseErrorKind::UnterminatedChar => d
                .with_label(self.span, "this character literal never closes")
                .with_help("add a closing `'`"),
            ParseErrorKind::UnterminatedComment => d
                .with_label(self.span, "this comment never closes")
                .with_note(
                    "a comment opened by N `#`s (N >= 2) spans multiple lines\nand is closed only by a run of exactly N `#`s",
                ),
            ParseErrorKind::UnterminatedGroup { open } => {
                let close = match open {
                    '(' => ')',
                    '[' => ']',
                    _ => '}',
                };
                d.with_label(self.span, format!("this `{open}` is never closed"))
                    .with_help(format!("add the matching `{close}`"))
            }
            ParseErrorKind::InvalidCharacter(c) => d
                .with_label(self.span, "not valid Omega syntax")
                .with_note(format!("the character is {:?} (U+{:04X})", c, *c as u32)),
            ParseErrorKind::InvalidMetavariable => d
                .with_label(self.span, "`$` must be followed by a name")
                .with_note("`$` has exactly one meaning: a macro metavariable, written `$name`"),
            ParseErrorKind::InvalidUnicodeEscape(_) => d
                .with_label(self.span, "not a valid Unicode scalar value")
                .with_note("valid scalar values are U+0000..=U+D7FF and U+E000..=U+10FFFF"),
            ParseErrorKind::InvalidCharLiteral => d
                .with_label(self.span, "must contain exactly one character")
                .with_help("write multi-character text as a string literal: `\"...\"`"),
        }
    }
}

/// A short, human-readable name for what was actually found at a failure
/// point -- built directly from a `TokenKind` by the lexer/parser, kept as
/// an owned `String` here (rather than borrowing a `Token`) so a
/// `ParseError` never needs to outlive the token stream it was produced
/// from.
pub type TokenDescription = String;

#[derive(Debug, Clone)]
pub enum ParseErrorKind {
    /// The general-purpose "this grammar rule didn't match" case, covering
    /// most parser call sites -- `expected` is a short, static description
    /// of what the parser was looking for (e.g. `"a type"`, `"';'"`,
    /// `"an expression"`).
    Expected { expected: &'static str, found: TokenDescription },
    UnterminatedString,
    UnterminatedChar,
    UnterminatedComment,
    /// A macro-body/argument capture (`{ ... }`/`( ... )`) never found its
    /// matching close delimiter before EOF.
    UnterminatedGroup { open: char },
    InvalidCharacter(char),
    /// A `$` not immediately followed by an identifier -- `$` has exactly
    /// one meaning in this grammar (a metavariable reference).
    InvalidMetavariable,
    InvalidUnicodeEscape(String),
    /// An empty character literal (`''`), or one containing more than one
    /// character/escape.
    InvalidCharLiteral,
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected { expected, found } => write!(f, "expected {expected}, found {found}"),
            Self::UnterminatedString => write!(f, "unterminated string literal"),
            Self::UnterminatedChar => write!(f, "unterminated character literal"),
            Self::UnterminatedComment => write!(f, "unterminated comment"),
            Self::UnterminatedGroup { open } => write!(f, "unterminated '{open}' (no matching close found)"),
            Self::InvalidCharacter(c) => write!(f, "unexpected character '{c}'"),
            Self::InvalidMetavariable => write!(f, "expected an identifier after '$'"),
            Self::InvalidUnicodeEscape(hex) => write!(f, "invalid unicode escape '\\u{{{hex}}}'"),
            Self::InvalidCharLiteral => write!(f, "character literal must contain exactly one character"),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}
