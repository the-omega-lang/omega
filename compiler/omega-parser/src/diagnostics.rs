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

    /// This error's complete renderable form -- headline, the caret label at
    /// the offending span, and any `note:`/`help:` footers.
    ///
    /// This is the **one** place any of an error's text lives. It used to be
    /// three: the variant's own doc comment, a `Display` impl holding the
    /// headline, and this method holding everything else -- three exhaustive
    /// matches in three different orders, with nothing keeping their wording
    /// in step. `Display` now reads its headline back from here (see below),
    /// so adding an error means adding exactly one arm.
    ///
    /// Advice is attached only where it is always true; a wrong hint is
    /// worse than none.
    pub fn to_diagnostic(&self) -> Diagnostic {
        match &self.kind {
            ParseErrorKind::Expected { expected, found } => Diagnostic::error(format!("expected {expected}, found {found}")).with_label(self.span, format!("expected {expected}")),
            ParseErrorKind::UnterminatedString => Diagnostic::error("unterminated string literal")
                .with_label(self.span, "this string never closes")
                .with_help("add a closing `\"`"),
            ParseErrorKind::UnterminatedChar => Diagnostic::error("unterminated character literal")
                .with_label(self.span, "this character literal never closes")
                .with_help("add a closing `'`"),
            ParseErrorKind::UnterminatedComment => Diagnostic::error("unterminated comment")
                .with_label(self.span, "this comment never closes")
                .with_note(
                    "a comment opened by N `#`s (N >= 2) spans multiple lines\nand is closed only by a run of exactly N `#`s",
                ),
            ParseErrorKind::EvenMultilineStringDelimiter { count } => Diagnostic::error(format!("multi-line string delimiter must use an odd number of quotes (found {count})"))
                .with_label(self.span, format!("this delimiter has {count} quotes, an even number"))
                .with_help("use an odd number of quotes (e.g. 3, 5, 7, ...) to open a multi-line string"),
            ParseErrorKind::UnterminatedGroup { open } => {
                let close = match open {
                    '(' => ')',
                    '[' => ']',
                    _ => '}',
                };
                Diagnostic::error(format!("unterminated '{open}' (no matching close found)")).with_label(self.span, format!("this `{open}` is never closed"))
                    .with_help(format!("add the matching `{close}`"))
            }
            ParseErrorKind::InvalidCharacter(c) => Diagnostic::error(format!("unexpected character '{c}'"))
                .with_label(self.span, "not valid Omega syntax")
                .with_note(format!("the character is {:?} (U+{:04X})", c, *c as u32)),
            ParseErrorKind::InvalidUnicodeEscape(hex) => Diagnostic::error(format!("invalid unicode escape '\\u{{{hex}}}'"))
                .with_label(self.span, "not a valid Unicode scalar value")
                .with_note("valid scalar values are U+0000..=U+D7FF and U+E000..=U+10FFFF"),
            ParseErrorKind::InvalidCharLiteral => Diagnostic::error("character literal must contain exactly one character")
                .with_label(self.span, "must contain exactly one character")
                .with_help("write multi-character text as a string literal: `\"...\"`"),
            ParseErrorKind::StructLiteralNotAllowedHere => Diagnostic::error("struct literals are not allowed in this position")
                .with_label(self.span, "the `{` here is ambiguous with the statement's own body")
                .with_help("wrap the struct literal in parentheses: `(Name { ... })`"),
            ParseErrorKind::EnumFunctionBeforeSemi => Diagnostic::error("enum functions must come after the variant list is ended with ';'")
                .with_label(self.span, "this looks like a function definition")
                .with_help("end the variant list with `;` before defining functions"),
            ParseErrorKind::EnumNotAllowedHere => Diagnostic::error("enums can only be declared at the top level of a module")
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `enum` to the module's top level"),
            ParseErrorKind::StructNotAllowedHere => Diagnostic::error("structs can only be declared at the top level of a module")
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `struct` to the module's top level"),
            ParseErrorKind::UnionNotAllowedHere => Diagnostic::error("unions can only be declared at the top level of a module")
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `union` to the module's top level"),
            ParseErrorKind::SpecNotAllowedHere => Diagnostic::error("specs can only be declared at the top level of a module")
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `spec` to the module's top level"),
            ParseErrorKind::SpecAliasCannotDeclareFunctions => Diagnostic::error("an alias spec can't declare its own functions")
                .with_label(self.span, "an alias spec (`= A | B`) can't declare its own functions")
                .with_help("give this spec a `{ ... }` body instead of `= ...` if it needs functions"),
            ParseErrorKind::RangeMissingEnd => Diagnostic::error("an inclusive ('..=') or exclusive ('..<') range must have an end bound")
                .with_label(self.span, "this range has no end bound")
                .with_help("give it an end (`..<end`/`..=end`), or use `..` if you mean an inferred, open-ended range"),
            ParseErrorKind::OpenRangeHasEnd => Diagnostic::error("an open range ('..') can't have an end")
                .with_label(self.span, "an open range ('..') can't have an end")
                .with_help("did you mean `..=end` (inclusive) or `..<end` (exclusive)?"),
            ParseErrorKind::NestingTooDeep { limit } => Diagnostic::error(format!("expression or type nests more than {limit} levels deep"))
                .with_label(self.span, format!("nesting goes deeper than {limit} levels here"))
                .with_note("the parser is recursive descent, so each level of nesting costs native stack -- this limit turns what would be a stack overflow into a diagnostic")
                .with_help("this is far past anything hand-written; if the source is generated, emit intermediate bindings instead of one deeply nested expression"),
            ParseErrorKind::AnnotationNotAllowedHere => Diagnostic::error("annotations are only allowed on structs, enums, unions, and functions")
                .with_label(self.span, "this item can't carry annotations")
                .with_help("annotations are only allowed on structs, enums, unions, and functions"),
            ParseErrorKind::VisibilityNotAllowedHere => Diagnostic::error("a visibility modifier is not allowed here")
                .with_label(self.span, "this item can't carry a visibility modifier")
                .with_help("'exposed'/'internal' are only allowed on structs, enums, unions, specs, macros, functions, globals, and externs"),
            ParseErrorKind::GapOrGlueVisibility => Diagnostic::error("gaps and glues take no visibility modifier")
                .with_label(self.span, "gaps and glues are global by nature")
                .with_help("remove this visibility modifier"),
            ParseErrorKind::ConformMethodVisibility => Diagnostic::error("a conforming method inherits its spec's visibility")
                .with_label(self.span, "a conforming method inherits its spec's visibility")
                .with_help("remove the method visibility modifier"),
            ParseErrorKind::PrimitiveVisibility => Diagnostic::error("a primitive block takes no visibility modifier")
                .with_label(self.span, "a primitive block does not declare the built-in type")
                .with_help("remove the block visibility modifier; put visibility on its functions"),
            ParseErrorKind::GapOrGlueGeneric => Diagnostic::error("gaps and glues cannot be generic")
                .with_label(self.span, "gaps and glues are never generic")
                .with_help("a gap's linker symbol is computed once, for the bare name -- there is no per-instantiation symbol to glue against"),
            ParseErrorKind::GapFunctionBody { name } => Diagnostic::error(format!("a gap declares, it does not define ('{}')",
                name.as_ref()))
                .with_label(self.span, format!("'{}' has a body", name.as_ref()))
                .with_note("a default body would need a real, once-compiled MIR function of its own, reusing the synthetic-`HirFunctionDef` reconstruction an ordinary spec default method already needs -- deferred, not ruled out")
                .with_help("declare it as a bare requirement (no body) instead -- the gap's one `glue` block must then provide it"),
            ParseErrorKind::GapFunctionSelf { name } => Diagnostic::error(format!("gap function '{}' cannot take 'self'", name.as_ref()))
                .with_label(self.span, format!("'{}' takes 'self'", name.as_ref()))
                .with_help("gap functions are static"),
            ParseErrorKind::DefaultGenericParamNotTrailing { name } => Diagnostic::error(format!("generic parameter '{name}' has no default, but an earlier one does"))
                .with_label(self.span, format!("`{name}` has no default, but an earlier parameter does"))
                .with_help("once one generic parameter has a default, every parameter after it must too"),
            ParseErrorKind::GlueFunctionShape { name } => Diagnostic::error(format!("glue function '{name}' must be non-generic and static"))
                .with_label(self.span, format!("`{name}` is generic or takes `self`"))
                .with_help("glue functions must be non-generic static functions"),
            ParseErrorKind::SpecDependenciesRemoved => Diagnostic::error("spec provisioning (`spec Name : Dep, Dep`) was removed from the language")
                .with_label(self.span, "spec provisioning was removed from the language")
                .with_note("a spec declares what its implementer provides, never a list of other specs to also satisfy")
                .with_help("name the combination with an alias (`spec X = A + B;`) or spell the conjunction at the bound (`<T: A + B>`), and conform each member separately"),
            ParseErrorKind::MacroInvocationNotAllowedAfterDefer => Diagnostic::error("a macro invocation can expand to more than one statement; write `defer { name$(...); }`")
                .with_label(self.span, "this invocation could expand to several statements")
                .with_help("write `defer { name$(...); }`"),
            ParseErrorKind::VariadicMacroParamNotLast => Diagnostic::error("a variadic macro parameter must be the last one, and a macro can have at most one")
                .with_label(self.span, "a variadic parameter ends the parameter list"),
            ParseErrorKind::InvalidMacroSeparator => Diagnostic::error("a macro repetition separator must be a single non-bracket token, e.g. `$...(,){ ... }`")
                .with_label(self.span, "a separator is exactly one non-bracket token"),
            ParseErrorKind::NestedMacroRepetition => Diagnostic::error("macro repetitions can't nest; a macro has at most one variadic parameter")
                .with_label(self.span, "a repetition cannot contain another repetition"),
            ParseErrorKind::ImportInMacroBody => Diagnostic::error("imports are not allowed in macro bodies")
                .with_label(self.span, "imports are not allowed in macro bodies")
                .with_note("macro-body names resolve in the macro's definition module")
                .with_help("import this name beside the macro definition instead"),
            ParseErrorKind::ChainedComparison => Diagnostic::error("comparison operators are non-associative")
                .with_label(self.span, "comparisons do not chain")
                .with_help("parenthesize the comparison you intend to evaluate first"),
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
    /// A second comparison operator after a non-associative comparison.
    ChainedComparison,
    /// Grammar nesting exceeded `parser::MAX_NESTING_DEPTH`. Reported once
    /// per module rather than per offending token: the parser refuses to
    /// descend, and block-level error recovery would otherwise re-enter and
    /// re-report the same overflow for every remaining statement.
    NestingTooDeep { limit: usize },
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
    /// error's own note in `ParseError::to_diagnostic` for what implementing
    /// one would take, and `docs/14-known-issues.md`.
    GapFunctionBody {
        name: Ident,
    },
    /// A `gap` function declared with any `self` at all -- gap functions
    /// are static, symbol-bound calls; there is no instance to hang a
    /// `self` off of.
    GapFunctionSelf {
        name: Ident,
    },
    /// A glue function is generic or takes `self`; glue functions are static.
    GlueFunctionShape { name: Ident },
    /// A generic parameter with no default followed one that does have one
    /// (`<T = i32, U>`) -- positional generic arguments make "explicit
    /// prefix, defaulted suffix" the only unambiguous omission shape, so
    /// this is rejected right where the full `<...>` list is known, before
    /// it ever reaches HIR. See `omega_parser::ast::generics::GenericParam`'s
    /// doc comment.
    DefaultGenericParamNotTrailing {
        name: Ident,
    },
    /// The removed `spec Name : Dep, Dep` provisioning form. A spec is a
    /// contract about what an implementer provides, never a list of other
    /// specs to also satisfy -- name the combination with an alias
    /// (`spec Name = A + B;`) or spell the conjunction at the bound
    /// (`<T: A + B>`), and conform each member separately.
    SpecDependenciesRemoved,
    MacroInvocationNotAllowedAfterDefer,
    VariadicMacroParamNotLast,
    InvalidMacroSeparator,
    NestedMacroRepetition,
    /// An `import` in a macro body would mutate the caller's namespace even
    /// though the body's own paths are definition-site resolved.
    ImportInMacroBody,
}

/// The headline only, read back from `ParseError::to_diagnostic` so the two
/// can never disagree. Used where there is no span to render against -- most
/// visibly `omega_parser::macros`, which joins parse failures from a
/// re-parsed expansion into one message.
impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = ParseError::new(Span::default(), self.clone()).to_diagnostic();
        f.write_str(&rendered.message)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}
