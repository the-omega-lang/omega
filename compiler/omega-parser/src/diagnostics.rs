use crate::ast::identifier::Ident;
use crate::ast::visibility::Visibility;
use omega_diagnostics::Diagnostic;
pub use omega_diagnostics::Span;
use std::fmt;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub kind: ParseErrorKind,
}

impl ParseError {
    pub fn new(span: Span, kind: ParseErrorKind) -> Self {
        Self { span, kind }
    }

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
            ParseErrorKind::AnnotationNotAllowedHere => Diagnostic::error("annotations are not allowed on this item")
                .with_label(self.span, "this item can't carry annotations"),
            ParseErrorKind::AnnotationWithoutItem => Diagnostic::error("annotation is not attached to an item")
                .with_label(self.span, "this annotation has no item")
                .with_help("add an item after the annotation or remove it"),
            ParseErrorKind::VisibilityNotAllowedHere => Diagnostic::error("a visibility modifier is not allowed here")
                .with_label(self.span, "this item can't carry a visibility modifier")
                .with_help("'exposed'/'shared' are only allowed on structs, enums, unions, specs, macros, functions, globals, and foreign items"),
            ParseErrorKind::GapOrGlueVisibility => Diagnostic::error("gaps and glues take no visibility modifier")
                .with_label(self.span, "gaps and glues are global by nature")
                .with_help("remove this visibility modifier"),
            ParseErrorKind::ConformMethodVisibility => Diagnostic::error("a conforming method inherits its spec's visibility")
                .with_label(self.span, "a conforming method inherits its spec's visibility")
                .with_help("remove the method visibility modifier"),
            ParseErrorKind::SpecMethodVisibilityExceedsSpec { member_visibility, spec_visibility } =>
                Diagnostic::error(format!("spec member visibility ('{member_visibility}') exceeds the spec's own visibility ('{spec_visibility}')"))
                .with_label(self.span, format!("'{member_visibility}' is more visible than the enclosing spec"))
                .with_help(format!("a spec member can only be as visible as its spec at most -- use '{spec_visibility}' or lower")),
            ParseErrorKind::PrimitiveVisibility => Diagnostic::error("a primitive block takes no visibility modifier")
                .with_label(self.span, "a primitive block does not declare the built-in type")
                .with_help("remove the block visibility modifier; put visibility on its functions"),
            ParseErrorKind::GapOrGlueGeneric => Diagnostic::error("gaps and glues cannot be generic")
                .with_label(self.span, "gaps and glues are never generic")
                .with_help("a gap's linker symbol is computed once, for the bare name -- there is no per-instantiation symbol to glue against"),
            ParseErrorKind::GapFunctionBody { name } => Diagnostic::error(format!("a gap declares, it does not define ('{}')",
                name.as_ref()))
                .with_label(self.span, format!("'{}' has a body", name.as_ref()))
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
                .with_help("spell the conjunction at the bound (`<T: A + B>`), and conform each member separately"),
            ParseErrorKind::MacroInvocationNotAllowedAfterDefer => Diagnostic::error("a macro invocation can expand to more than one statement; write `defer { name$(...); }`")
                .with_label(self.span, "this invocation could expand to several statements")
                .with_help("write `defer { name$(...); }`"),
            ParseErrorKind::VariadicMacroParamNotLast => Diagnostic::error("a variadic macro parameter must be the last one, and a macro can have at most one")
                .with_label(self.span, "a variadic parameter ends the parameter list"),
            ParseErrorKind::DuplicateMacroParam { name } => Diagnostic::error(format!("macro parameter '{name}' is declared more than once"))
                .with_label(self.span, format!("duplicate parameter '{name}'")),
            ParseErrorKind::InvalidMacroSeparator => Diagnostic::error("a macro repetition separator must be a single non-bracket token, e.g. `$...(,){ ... }`")
                .with_label(self.span, "a separator is exactly one non-bracket token"),
            ParseErrorKind::NestedMacroRepetition => Diagnostic::error("macro repetitions can't nest; a macro has at most one variadic parameter")
                .with_label(self.span, "a repetition cannot contain another repetition"),
            ParseErrorKind::ImportInMacroBody => Diagnostic::error("imports are not allowed in macro bodies")
                .with_label(self.span, "imports are not allowed in macro bodies")
                .with_note("macro-body names resolve in the macro's definition module")
                .with_help("import this name beside the macro definition instead"),
            ParseErrorKind::UnterminatedAsmBody => Diagnostic::error("unterminated inline assembly body")
                .with_label(self.span, "this asm body never closes")
                .with_help("add a closing `}` that matches the opening `{` after `=>`"),
            ParseErrorKind::ChainedComparison => Diagnostic::error("comparison operators are non-associative")
                .with_label(self.span, "comparisons do not chain")
                .with_help("parenthesize the comparison you intend to evaluate first"),
            ParseErrorKind::ForeignConventionOnBinding => Diagnostic::error("a foreign binding cannot carry its own calling convention")
                .with_label(self.span, "'foreign(cc)' is not allowed directly on a 'name : Type' binding")
                .with_help("write 'foreign name : foreign(cc) (...) => T;' instead -- the convention belongs to the type"),
            ParseErrorKind::NestedForeignBlock => Diagnostic::error("foreign blocks cannot nest")
                .with_label(self.span, "this 'foreign' block is inside another foreign block")
                .with_help("flatten this into a direct entry of the enclosing block"),
        }
    }
}

pub type TokenDescription = String;

#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    Expected {
        expected: &'static str,
        found: TokenDescription,
    },
    UnterminatedString,
    UnterminatedChar,
    UnterminatedComment,
    EvenMultilineStringDelimiter {
        count: usize,
    },
    UnterminatedGroup {
        open: char,
    },
    InvalidCharacter(char),
    InvalidUnicodeEscape(String),
    InvalidCharLiteral,
    StructLiteralNotAllowedHere,
    EnumFunctionBeforeSemi,
    EnumNotAllowedHere,
    StructNotAllowedHere,
    UnionNotAllowedHere,
    SpecNotAllowedHere,
    RangeMissingEnd,
    OpenRangeHasEnd,
    ChainedComparison,
    NestingTooDeep {
        limit: usize,
    },
    AnnotationNotAllowedHere,
    AnnotationWithoutItem,
    VisibilityNotAllowedHere,
    GapOrGlueVisibility,
    ConformMethodVisibility,
    SpecMethodVisibilityExceedsSpec {
        member_visibility: Visibility,
        spec_visibility: Visibility,
    },
    PrimitiveVisibility,
    GapOrGlueGeneric,
    GapFunctionBody {
        name: Ident,
    },
    GapFunctionSelf {
        name: Ident,
    },
    GlueFunctionShape {
        name: Ident,
    },
    DefaultGenericParamNotTrailing {
        name: Ident,
    },
    SpecDependenciesRemoved,
    MacroInvocationNotAllowedAfterDefer,
    VariadicMacroParamNotLast,
    DuplicateMacroParam {
        name: Ident,
    },
    InvalidMacroSeparator,
    NestedMacroRepetition,
    ImportInMacroBody,
    UnterminatedAsmBody,
    ForeignConventionOnBinding,
    NestedForeignBlock,
}

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
