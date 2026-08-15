//! Parse-time error types, and their conversion into renderable
//! [`Diagnostic`]s. The position/rendering machinery itself
//! ([`Span`], `SourceFile`, `Renderer`) lives in `omega_diagnostics` -- this
//! module only owns what a *parser* knows: which grammar rule failed, and
//! what advice helps fix it.

use crate::ast::identifier::Ident;
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
            ParseErrorKind::EvenMultilineStringDelimiter { count } => d
                .with_label(self.span, format!("this delimiter has {count} quotes, an even number"))
                .with_help("use an odd number of quotes (e.g. 3, 5, 7, ...) to open a multi-line string"),
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
            ParseErrorKind::InvalidUnicodeEscape(_) => d
                .with_label(self.span, "not a valid Unicode scalar value")
                .with_note("valid scalar values are U+0000..=U+D7FF and U+E000..=U+10FFFF"),
            ParseErrorKind::InvalidCharLiteral => d
                .with_label(self.span, "must contain exactly one character")
                .with_help("write multi-character text as a string literal: `\"...\"`"),
            ParseErrorKind::StructLiteralNotAllowedHere => d
                .with_label(self.span, "the `{` here is ambiguous with the statement's own body")
                .with_help("wrap the struct literal in parentheses: `(Name { ... })`"),
            ParseErrorKind::EnumFunctionBeforeSemi => d
                .with_label(self.span, "this looks like a function definition")
                .with_help("end the variant list with `;` before defining functions"),
            ParseErrorKind::EnumNotAllowedHere => d
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `enum` to the module's top level"),
            ParseErrorKind::StructNotAllowedHere => d
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `struct` to the module's top level"),
            ParseErrorKind::UnionNotAllowedHere => d
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `union` to the module's top level"),
            ParseErrorKind::SpecNotAllowedHere => d
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `spec` to the module's top level"),
            ParseErrorKind::SpecAliasCannotDeclareFunctions => d
                .with_label(self.span, "an alias spec (`= A | B`) can't declare its own functions")
                .with_help("give this spec a `{ ... }` body instead of `= ...` if it needs functions"),
            ParseErrorKind::RangeMissingEnd => d
                .with_label(self.span, "this range has no end bound")
                .with_help("give it an end (`..<end`/`..=end`), or use `..` if you mean an inferred, open-ended range"),
            ParseErrorKind::OpenRangeHasEnd => d
                .with_label(self.span, "an open range ('..') can't have an end")
                .with_help("did you mean `..=end` (inclusive) or `..<end` (exclusive)?"),
            ParseErrorKind::AnnotationNotAllowedHere => d
                .with_label(self.span, "this item can't carry annotations")
                .with_help("annotations are only allowed on structs, enums, unions, and functions"),
            ParseErrorKind::VisibilityNotAllowedHere => d
                .with_label(self.span, "this item can't carry a visibility modifier")
                .with_help("'exposed'/'internal' are only allowed on structs, enums, unions, specs, macros, functions, globals, and externs"),
            ParseErrorKind::GapOrGlueVisibility => d
                .with_label(self.span, "gaps and glues are global by nature")
                .with_help("remove this visibility modifier"),
            ParseErrorKind::ConformMethodVisibility => d
                .with_label(self.span, "a conforming method inherits its spec's visibility")
                .with_help("remove the method visibility modifier"),
            ParseErrorKind::PrimitiveVisibility => d
                .with_label(self.span, "a primitive block does not declare the built-in type")
                .with_help("remove the block visibility modifier; put visibility on its functions"),
            ParseErrorKind::GapOrGlueGeneric => d
                .with_label(self.span, "gaps and glues are never generic")
                .with_help("a gap's linker symbol is computed once, for the bare name -- there is no per-instantiation symbol to glue against"),
            ParseErrorKind::GapFunctionBody { name } => d
                .with_label(self.span, format!("'{}' has a body", name.as_ref()))
                .with_note("a default body would need a real, once-compiled MIR function of its own, reusing the synthetic-`HirFunctionDef` reconstruction an ordinary spec default method already needs -- deferred, not ruled out")
                .with_help("declare it as a bare requirement (no body) instead -- the gap's one `glue` block must then provide it"),
            ParseErrorKind::GapFunctionSelf { name } => d
                .with_label(self.span, format!("'{}' takes 'self'", name.as_ref()))
                .with_help("gap functions are static"),
            ParseErrorKind::DefaultGenericParamNotTrailing { name } => d
                .with_label(self.span, format!("`{name}` has no default, but an earlier parameter does"))
                .with_help("once one generic parameter has a default, every parameter after it must too"),
            ParseErrorKind::MacroInvocationNotAllowedAfterDefer => d
                .with_label(self.span, "this invocation could expand to several statements")
                .with_help("write `defer { name$(...); }`"),
            ParseErrorKind::VariadicMacroParamNotLast => d
                .with_label(self.span, "a variadic parameter ends the parameter list"),
            ParseErrorKind::InvalidMacroSeparator => d
                .with_label(self.span, "a separator is exactly one non-bracket token"),
            ParseErrorKind::NestedMacroRepetition => d
                .with_label(self.span, "a repetition cannot contain another repetition"),
            ParseErrorKind::ImportInMacroBody => d
                .with_label(self.span, "imports are not allowed in macro bodies")
                .with_note("macro-body names resolve in the macro's definition module")
                .with_help("import this name beside the macro definition instead"),
        }
    }
}

/// A short, human-readable name for what was actually found at a failure
/// point -- built directly from a `TokenKind` by the lexer/parser, kept as
/// an owned `String` here (rather than borrowing a `Token`) so a
/// `ParseError` never needs to outlive the token stream it was produced
/// from.
pub type TokenDescription = String;

#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    /// The general-purpose "this grammar rule didn't match" case, covering
    /// most parser call sites -- `expected` is a short, static description
    /// of what the parser was looking for (e.g. `"a type"`, `"';'"`,
    /// `"an expression"`).
    Expected {
        expected: &'static str,
        found: TokenDescription,
    },
    UnterminatedString,
    UnterminatedChar,
    UnterminatedComment,
    /// A multi-line string's opening delimiter (`"""..."""`-style, N >= 3
    /// quotes) used an even `count` -- disallowed, since an even-length
    /// delimiter could otherwise be confused with two shorter,
    /// separately-closed runs; see `Lexer::scan_string_or_multiline`.
    /// Recorded but non-fatal: lexing still produces a best-effort `Str`
    /// token (searching for a closing run of the same `count` regardless)
    /// so a single malformed delimiter doesn't cascade into unrelated
    /// downstream errors.
    EvenMultilineStringDelimiter {
        count: usize,
    },
    /// A macro-body/argument capture (`{ ... }`/`( ... )`) never found its
    /// matching close delimiter before EOF.
    UnterminatedGroup {
        open: char,
    },
    InvalidCharacter(char),
    InvalidUnicodeEscape(String),
    /// An empty character literal (`''`), or one containing more than one
    /// character/escape.
    InvalidCharLiteral,
    /// A struct literal written directly in `if`/`while`/`for` condition
    /// position, where its `{` would be ambiguous with the statement's own
    /// body block -- only reported when the speculative parse is *sure*
    /// (see `parser::expression::parse_primary`'s restricted-`Ident` case),
    /// never on a mere possibility.
    StructLiteralNotAllowedHere,
    /// A function definition where an enum variant was expected -- the
    /// variant list must be ended with `;` before functions can follow
    /// (see `parser::item::parse_enum_def`).
    EnumFunctionBeforeSemi,
    /// An `enum` declaration in statement position -- enums are top-level
    /// items only.
    EnumNotAllowedHere,
    /// A `struct` declaration in statement position -- structs are
    /// top-level items only: a locally-nested one would bypass the
    /// driver's whole module-level query/cache/cycle-detection system, with
    /// no forward-reference or cross-item support and no working
    /// self-reference-cycle guard.
    StructNotAllowedHere,
    /// A `union` declaration in statement position -- same reasoning as
    /// `StructNotAllowedHere`.
    UnionNotAllowedHere,
    /// A `spec` declaration in statement position -- same reasoning as
    /// `StructNotAllowedHere`.
    SpecNotAllowedHere,
    /// `spec Name = A | B { ... }` -- the alias form is pure union syntax
    /// sugar (see `ast::statement::spec::SpecStmt`'s doc comment) and can't
    /// carry its own function members the way a `spec Name : A, B { ... }`
    /// declaration can.
    SpecAliasCannotDeclareFunctions,
    /// `..<`/`..=` with no end bound (`a..<`/`a..=`, or bare `..<`/`..=`)
    /// -- unlike `..`, an inclusive or exclusive range's whole point is a
    /// specific end, so an open-ended one is meaningless; write bare `..`
    /// instead if that's really what's meant. See `ast::range::RangeExpr`.
    RangeMissingEnd,
    /// `..` immediately followed by something other than the range's own
    /// terminator (`]` for a slice, `=>` for a match pattern, `{` for a
    /// range-driven `for` loop's body) -- `..` never takes an end at all;
    /// this almost always means `..=`/`..<` was meant instead.
    OpenRangeHasEnd,
    /// One or more `@name(...)` annotations directly above an item that has
    /// nowhere to store them -- only structs, enums, unions, and functions
    /// (top-level or member) carry an `annotations` list at all; annotating
    /// an `extern`/`import`/plain declaration/macro is rejected here rather
    /// than silently dropped.
    AnnotationNotAllowedHere,
    /// A leading `exposed`/`internal` directly above an item that has
    /// nowhere to store a visibility (`import`/macro definition/macro
    /// invocation) -- rejected here rather than silently dropped, same
    /// precedent as `AnnotationNotAllowedHere`.
    VisibilityNotAllowedHere,
    GapOrGlueVisibility,
    ConformMethodVisibility,
    PrimitiveVisibility,
    /// A `<...>` list on a `gap` name or a `glue` target path. Reported
    /// (and then *consumed* by the caller, see `parse_gap_def`) rather than
    /// aborting the item, so the one real mistake produces one error
    /// instead of a cascade from re-reading `<T>` as a fresh top-level item.
    GapOrGlueGeneric,
    /// A `gap` function written with a body. Default-bodied gap functions
    /// are a deliberately deferred *feature*, not a shape rule -- see this
    /// error's own note in `ParseError::render` for what implementing one
    /// would take, and `docs/14-known-issues.md`.
    GapFunctionBody {
        name: Ident,
    },
    /// A `gap` function declared with any `self` at all -- gap functions
    /// are static, symbol-bound calls; there is no instance to hang a
    /// `self` off of.
    GapFunctionSelf {
        name: Ident,
    },
    /// A generic parameter with no default followed one that does have one
    /// (`<T = i32, U>`) -- positional generic arguments make "explicit
    /// prefix, defaulted suffix" the only unambiguous omission shape, so
    /// this is rejected right where the full `<...>` list is known, before
    /// it ever reaches HIR. See `omega_parser::ast::generics::GenericParam`'s
    /// doc comment.
    DefaultGenericParamNotTrailing {
        name: Ident,
    },
    MacroInvocationNotAllowedAfterDefer,
    VariadicMacroParamNotLast,
    InvalidMacroSeparator,
    NestedMacroRepetition,
    /// An `import` in a macro body would mutate the caller's namespace even
    /// though the body's own paths are definition-site resolved.
    ImportInMacroBody,
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected { expected, found } => write!(f, "expected {expected}, found {found}"),
            Self::UnterminatedString => write!(f, "unterminated string literal"),
            Self::UnterminatedChar => write!(f, "unterminated character literal"),
            Self::UnterminatedComment => write!(f, "unterminated comment"),
            Self::EvenMultilineStringDelimiter { count } => {
                write!(
                    f,
                    "multi-line string delimiter must use an odd number of quotes (found {count})"
                )
            }
            Self::UnterminatedGroup { open } => {
                write!(f, "unterminated '{open}' (no matching close found)")
            }
            Self::InvalidCharacter(c) => write!(f, "unexpected character '{c}'"),
            Self::InvalidUnicodeEscape(hex) => write!(f, "invalid unicode escape '\\u{{{hex}}}'"),
            Self::InvalidCharLiteral => {
                write!(f, "character literal must contain exactly one character")
            }
            Self::StructLiteralNotAllowedHere => {
                write!(f, "struct literals are not allowed in this position")
            }
            Self::EnumFunctionBeforeSemi => {
                write!(
                    f,
                    "enum functions must come after the variant list is ended with ';'"
                )
            }
            Self::EnumNotAllowedHere => {
                write!(f, "enums can only be declared at the top level of a module")
            }
            Self::StructNotAllowedHere => {
                write!(
                    f,
                    "structs can only be declared at the top level of a module"
                )
            }
            Self::UnionNotAllowedHere => {
                write!(
                    f,
                    "unions can only be declared at the top level of a module"
                )
            }
            Self::SpecNotAllowedHere => {
                write!(f, "specs can only be declared at the top level of a module")
            }
            Self::SpecAliasCannotDeclareFunctions => {
                write!(f, "an alias spec can't declare its own functions")
            }
            Self::RangeMissingEnd => {
                write!(
                    f,
                    "an inclusive ('..=') or exclusive ('..<') range must have an end bound"
                )
            }
            Self::OpenRangeHasEnd => {
                write!(f, "an open range ('..') can't have an end")
            }
            Self::AnnotationNotAllowedHere => {
                write!(
                    f,
                    "annotations are only allowed on structs, enums, unions, and functions"
                )
            }
            Self::VisibilityNotAllowedHere => {
                write!(f, "a visibility modifier is not allowed here")
            }
            Self::GapOrGlueVisibility => write!(f, "gaps and glues take no visibility modifier"),
            Self::ConformMethodVisibility => {
                write!(f, "a conforming method inherits its spec's visibility")
            }
            Self::PrimitiveVisibility => {
                write!(f, "a primitive block takes no visibility modifier")
            }
            Self::GapOrGlueGeneric => write!(f, "gaps and glues cannot be generic"),
            Self::GapFunctionBody { name } => write!(
                f,
                "a gap declares, it does not define ('{}')",
                name.as_ref()
            ),
            Self::GapFunctionSelf { name } => {
                write!(f, "gap function '{}' cannot take 'self'", name.as_ref())
            }
            Self::DefaultGenericParamNotTrailing { name } => {
                write!(
                    f,
                    "generic parameter '{name}' has no default, but an earlier one does"
                )
            }
            Self::MacroInvocationNotAllowedAfterDefer => write!(
                f,
                "a macro invocation can expand to more than one statement; write `defer {{ name$(...); }}`"
            ),
            Self::VariadicMacroParamNotLast => write!(
                f,
                "a variadic macro parameter must be the last one, and a macro can have at most one"
            ),
            Self::InvalidMacroSeparator => write!(
                f,
                "a macro repetition separator must be a single non-bracket token, e.g. `$...(,){{ ... }}`"
            ),
            Self::NestedMacroRepetition => write!(
                f,
                "macro repetitions can't nest; a macro has at most one variadic parameter"
            ),
            Self::ImportInMacroBody => write!(f, "imports are not allowed in macro bodies"),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}
