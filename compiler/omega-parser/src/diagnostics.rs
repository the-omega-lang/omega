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
            ParseErrorKind::Expected { expected, .. } => Diagnostic::error(self.kind.message()).with_label(self.span, format!("expected {expected}")),
            ParseErrorKind::UnterminatedString => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this string never closes")
                .with_help("add a closing `\"`"),
            ParseErrorKind::UnterminatedChar => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this character literal never closes")
                .with_help("add a closing `'`"),
            ParseErrorKind::UnterminatedComment => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this comment never closes")
                .with_note(
                    "a comment opened by N `#`s (N >= 2) spans multiple lines\nand is closed only by a run of exactly N `#`s",
                ),
            ParseErrorKind::EvenMultilineStringDelimiter { count } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("this delimiter has {count} quotes, an even number"))
                .with_help("use an odd number of quotes (e.g. 3, 5, 7, ...) to open a multi-line string"),
            ParseErrorKind::UnterminatedGroup { open } => {
                let close = match open {
                    '(' => ')',
                    '[' => ']',
                    _ => '}',
                };
                Diagnostic::error(self.kind.message()).with_label(self.span, format!("this `{open}` is never closed"))
                    .with_help(format!("add the matching `{close}`"))
            }
            ParseErrorKind::InvalidCharacter(c) => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not valid Omega syntax")
                .with_note(format!("the character is {:?} (U+{:04X})", c, *c as u32)),
            ParseErrorKind::InvalidUnicodeEscape(_) => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not a valid Unicode scalar value")
                .with_note("valid scalar values are U+0000..=U+D7FF and U+E000..=U+10FFFF"),
            ParseErrorKind::NumberSuffixNeedsSeparator { suggestion, .. } => Diagnostic::error(self.kind.message())
                .with_label(self.span, "no visible boundary between the digits and the suffix")
                .with_note("hexadecimal digits include letters, so an attached suffix is unreadable")
                .with_help(format!("write it as `{suggestion}`")),
            ParseErrorKind::InvalidCharLiteral => Diagnostic::error(self.kind.message())
                .with_label(self.span, "must contain exactly one character")
                .with_help("write multi-character text as a string literal: `\"...\"`"),
            ParseErrorKind::StructLiteralNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "the `{` here is ambiguous with the statement's own body")
                .with_help("wrap the struct literal in parentheses: `(Name { ... })`"),
            ParseErrorKind::EnumFunctionBeforeSemi => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this looks like a function definition")
                .with_help("end the variant list with `;` before defining functions"),
            ParseErrorKind::EnumNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `enum` to the module's top level"),
            ParseErrorKind::StructNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `struct` to the module's top level"),
            ParseErrorKind::UnionNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `union` to the module's top level"),
            ParseErrorKind::SpecNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `spec` to the module's top level"),
            ParseErrorKind::AliasNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "not allowed inside a function body")
                .with_help("move this `alias` to the module's top level"),
            ParseErrorKind::RangeMissingEnd => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this range has no end bound")
                .with_help("give it an end (`..<end`/`..=end`), or use `..` if you mean an inferred, open-ended range"),
            ParseErrorKind::OpenRangeHasEnd => Diagnostic::error(self.kind.message())
                .with_label(self.span, "an open range ('..') can't have an end")
                .with_help("did you mean `..=end` (inclusive) or `..<end` (exclusive)?"),
            ParseErrorKind::InferenceHoleNotAValue => Diagnostic::error(self.kind.message())
                .with_label(self.span, "`_` cannot be used as an expression")
                .with_note("`_` is a reserved token that stands for a type or value the compiler infers"),
            ParseErrorKind::ReceiverInFunctionType { spelling } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("`{spelling}` is only valid in a method declaration"))
                .with_help("write the receiver's type as an explicit first parameter, such as `(*Thing) => void`"),
            ParseErrorKind::NestingTooDeep { limit } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("nesting goes deeper than {limit} levels here"))
                .with_note("the parser is recursive descent, so each level of nesting costs native stack -- this limit turns what would be a stack overflow into a diagnostic")
                .with_help("this is far past anything hand-written; if the source is generated, emit intermediate bindings instead of one deeply nested expression"),
            ParseErrorKind::AnnotationNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this item can't carry annotations"),
            ParseErrorKind::SourceAnnotationNotInPrologue => Diagnostic::error(self.kind.message())
                .with_label(self.span, "source-level annotation outside the source prologue"),
            ParseErrorKind::ConditionNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this is not a top-level item")
                .with_help("a condition selects a whole declaration, so put it on the declaration this belongs to"),
            ParseErrorKind::AnnotationWithoutItem => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this annotation has no item")
                .with_help("add an item after the annotation or remove it"),
            ParseErrorKind::VisibilityNotAllowedHere => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this item can't carry a visibility modifier")
                .with_help("'exposed'/'shared' are only allowed on structs, enums, unions, specs, macros, functions, globals, and foreign items"),
            ParseErrorKind::GapOrGlueVisibility => Diagnostic::error(self.kind.message())
                .with_label(self.span, "gaps and glues are global by nature")
                .with_help("remove this visibility modifier"),
            ParseErrorKind::ConformMethodVisibility => Diagnostic::error(self.kind.message())
                .with_label(self.span, "a conforming method inherits its spec's visibility")
                .with_help("remove the method visibility modifier"),
            ParseErrorKind::SpecMethodVisibilityExceedsSpec { member_visibility, spec_visibility } =>
                Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("'{member_visibility}' is more visible than the enclosing spec"))
                .with_help(format!("a spec member can only be as visible as its spec at most -- use '{spec_visibility}' or lower")),
            ParseErrorKind::PrimitiveVisibility => Diagnostic::error(self.kind.message())
                .with_label(self.span, "a primitive block does not declare the built-in type")
                .with_help("remove the block visibility modifier; put visibility on its functions"),
            ParseErrorKind::GapOrGlueGeneric => Diagnostic::error(self.kind.message())
                .with_label(self.span, "gaps and glues are never generic")
                .with_help("a gap's linker symbol is computed once, for the bare name -- there is no per-instantiation symbol to glue against"),
            ParseErrorKind::GapFunctionBody { name } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("'{}' has a body", name.as_ref()))
                .with_help("declare it as a bare requirement (no body) instead -- the gap's one `glue` block must then provide it"),
            ParseErrorKind::GapFunctionSelf { name } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("'{}' takes 'self'", name.as_ref()))
                .with_help("gap functions are static"),
            ParseErrorKind::DefaultGenericParamNotTrailing { name } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("`{name}` has no default, but an earlier parameter does"))
                .with_help("once one generic parameter has a default, every parameter after it must too"),
            ParseErrorKind::GlueFunctionShape { name } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("`{name}` is generic or takes `self`"))
                .with_help("glue functions must be non-generic static functions"),
            ParseErrorKind::SpecDependenciesRemoved => Diagnostic::error(self.kind.message())
                .with_label(self.span, "spec provisioning was removed from the language")
                .with_note("a spec declares what its implementer provides, never a list of other specs to also satisfy")
                .with_help("spell the conjunction at the bound (`<T: A + B>`), and conform each member separately"),
            ParseErrorKind::MacroInvocationNotAllowedAfterDefer => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this invocation could expand to several statements")
                .with_help("write `defer { name$(...); }`"),
            ParseErrorKind::VariadicMacroParamNotLast => Diagnostic::error(self.kind.message())
                .with_label(self.span, "a variadic parameter ends the parameter list"),
            ParseErrorKind::DuplicateMacroParam { name } => Diagnostic::error(self.kind.message())
                .with_label(self.span, format!("duplicate parameter '{name}'")),
            ParseErrorKind::InvalidMacroSeparator => Diagnostic::error(self.kind.message())
                .with_label(self.span, "a separator is exactly one token, and not a bracket, `$`, or a metavariable"),
            ParseErrorKind::NestedMacroRepetition => Diagnostic::error(self.kind.message())
                .with_label(self.span, "a repetition cannot contain another repetition"),
            ParseErrorKind::ImportInMacroBody => Diagnostic::error(self.kind.message())
                .with_label(self.span, "imports are not allowed in macro bodies")
                .with_note("macro-body names resolve in the macro's definition module")
                .with_help("import this name beside the macro definition instead"),
            ParseErrorKind::EmptyImportGroup => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this group is empty")
                .with_help("write the names to import inside the braces, or drop the group"),
            ParseErrorKind::ImportAliasOnPrefix => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this 'as' renames a path prefix, not a binding")
                .with_help("rename each imported name inside the group instead"),
            ParseErrorKind::ImportSelfNotTerminal => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this 'self' is followed by more path segments")
                .with_help("write the segments as their own group entry, or use 'self' alone to bind the prefix"),
            ParseErrorKind::UnterminatedAsmBody => Diagnostic::error(self.kind.message())
                .with_label(self.span, "this asm body never closes")
                .with_help("add a closing `}` that matches the opening `{` after `=>`"),
            ParseErrorKind::ChainedComparison => Diagnostic::error(self.kind.message())
                .with_label(self.span, "comparisons do not chain")
                .with_help("parenthesize the comparison you intend to evaluate first"),
            ParseErrorKind::ForeignConventionOnBinding => Diagnostic::error(self.kind.message())
                .with_label(self.span, "'foreign(cc)' is not allowed directly on a 'name : Type' binding")
                .with_help("write 'foreign name : foreign(cc) (...) => T;' instead -- the convention belongs to the type"),
            ParseErrorKind::ForeignMutOnFunction => Diagnostic::error(self.kind.message())
                .with_label(self.span, "'mut' applies to data storage, not a function symbol")
                .with_help("remove 'mut' from this function declaration or definition"),
            ParseErrorKind::NestedForeignBlock => Diagnostic::error(self.kind.message())
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
    NumberSuffixNeedsSeparator {
        suffix: Ident,
        suggestion: String,
    },
    InvalidCharLiteral,
    StructLiteralNotAllowedHere,
    EnumFunctionBeforeSemi,
    EnumNotAllowedHere,
    StructNotAllowedHere,
    UnionNotAllowedHere,
    SpecNotAllowedHere,
    AliasNotAllowedHere,
    RangeMissingEnd,
    OpenRangeHasEnd,
    InferenceHoleNotAValue,
    ReceiverInFunctionType {
        spelling: String,
    },
    ChainedComparison,
    NestingTooDeep {
        limit: usize,
    },
    AnnotationNotAllowedHere,
    ConditionNotAllowedHere,
    SourceAnnotationNotInPrologue,
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
    EmptyImportGroup,
    ImportAliasOnPrefix,
    ImportSelfNotTerminal,
    UnterminatedAsmBody,
    ForeignConventionOnBinding,
    ForeignMutOnFunction,
    NestedForeignBlock,
}

impl ParseErrorKind {
    /// The headline text, which `ParseError::to_diagnostic` also uses.
    pub fn message(&self) -> String {
        match self {
            ParseErrorKind::Expected { expected, found } => format!("expected {expected}, found {found}"),
            ParseErrorKind::UnterminatedString => "unterminated string literal".to_owned(),
            ParseErrorKind::UnterminatedChar => "unterminated character literal".to_owned(),
            ParseErrorKind::UnterminatedComment => "unterminated comment".to_owned(),
            ParseErrorKind::EvenMultilineStringDelimiter { count } => format!("multi-line string delimiter must use an odd number of quotes (found {count})"),
            ParseErrorKind::UnterminatedGroup { open } => format!("unterminated '{open}' (no matching close found)"),
            ParseErrorKind::InvalidCharacter(c) => format!("unexpected character '{c}'"),
            ParseErrorKind::InvalidUnicodeEscape(hex) => format!("invalid unicode escape '\\u{{{hex}}}'"),
            ParseErrorKind::NumberSuffixNeedsSeparator { suffix, .. } => format!("type suffix `{suffix}` must be separated from a hexadecimal literal by `_`"),
            ParseErrorKind::InvalidCharLiteral => "character literal must contain exactly one character".to_owned(),
            ParseErrorKind::StructLiteralNotAllowedHere => "struct literals are not allowed in this position".to_owned(),
            ParseErrorKind::EnumFunctionBeforeSemi => "enum functions must come after the variant list is ended with ';'".to_owned(),
            ParseErrorKind::EnumNotAllowedHere => "enums can only be declared at the top level of a module".to_owned(),
            ParseErrorKind::StructNotAllowedHere => "structs can only be declared at the top level of a module".to_owned(),
            ParseErrorKind::UnionNotAllowedHere => "unions can only be declared at the top level of a module".to_owned(),
            ParseErrorKind::SpecNotAllowedHere => "specs can only be declared at the top level of a module".to_owned(),
            ParseErrorKind::AliasNotAllowedHere => "aliases can only be declared at the top level of a module".to_owned(),
            ParseErrorKind::RangeMissingEnd => "an inclusive ('..=') or exclusive ('..<') range must have an end bound".to_owned(),
            ParseErrorKind::OpenRangeHasEnd => "an open range ('..') can't have an end".to_owned(),
            ParseErrorKind::InferenceHoleNotAValue => "'_' is an inference hole, not a name or a value".to_owned(),
            ParseErrorKind::ReceiverInFunctionType { .. } => "a function type has no receiver".to_owned(),
            ParseErrorKind::NestingTooDeep { limit } => format!("expression or type nests more than {limit} levels deep"),
            ParseErrorKind::AnnotationNotAllowedHere => "annotations are not allowed on this item".to_owned(),
            ParseErrorKind::SourceAnnotationNotInPrologue => "source-level annotations must come before any top-level declaration".to_owned(),
            ParseErrorKind::ConditionNotAllowedHere => "'@cond' is only allowed on a top-level item".to_owned(),
            ParseErrorKind::AnnotationWithoutItem => "annotation is not attached to an item".to_owned(),
            ParseErrorKind::VisibilityNotAllowedHere => "a visibility modifier is not allowed here".to_owned(),
            ParseErrorKind::GapOrGlueVisibility => "gaps and glues take no visibility modifier".to_owned(),
            ParseErrorKind::ConformMethodVisibility => "a conforming method inherits its spec's visibility".to_owned(),
            ParseErrorKind::SpecMethodVisibilityExceedsSpec { member_visibility, spec_visibility } => format!("spec member visibility ('{member_visibility}') exceeds the spec's own visibility ('{spec_visibility}')"),
            ParseErrorKind::PrimitiveVisibility => "a primitive block takes no visibility modifier".to_owned(),
            ParseErrorKind::GapOrGlueGeneric => "gaps and glues cannot be generic".to_owned(),
            ParseErrorKind::GapFunctionBody { name } => format!("a gap declares, it does not define ('{}')",
                name.as_ref()),
            ParseErrorKind::GapFunctionSelf { name } => format!("gap function '{}' cannot take 'self'", name.as_ref()),
            ParseErrorKind::DefaultGenericParamNotTrailing { name } => format!("generic parameter '{name}' has no default, but an earlier one does"),
            ParseErrorKind::GlueFunctionShape { name } => format!("glue function '{name}' must be non-generic and static"),
            ParseErrorKind::SpecDependenciesRemoved => "spec provisioning (`spec Name : Dep, Dep`) was removed from the language".to_owned(),
            ParseErrorKind::MacroInvocationNotAllowedAfterDefer => "a macro invocation can expand to more than one statement; write `defer { name$(...); }`".to_owned(),
            ParseErrorKind::VariadicMacroParamNotLast => "a variadic macro parameter must be the last one, and a macro can have at most one".to_owned(),
            ParseErrorKind::DuplicateMacroParam { name } => format!("macro parameter '{name}' is declared more than once"),
            ParseErrorKind::InvalidMacroSeparator => "a macro repetition separator must be a single token other than a bracket, `$`, or a metavariable, e.g. `$...(,){ ... }`".to_owned(),
            ParseErrorKind::NestedMacroRepetition => "macro repetitions can't nest; a macro has at most one variadic parameter".to_owned(),
            ParseErrorKind::ImportInMacroBody => "imports are not allowed in macro bodies".to_owned(),
            ParseErrorKind::EmptyImportGroup => "an import group must import at least one name".to_owned(),
            ParseErrorKind::ImportAliasOnPrefix => "only a complete import binding can be renamed with 'as'".to_owned(),
            ParseErrorKind::ImportSelfNotTerminal => "'self' in an import group binds the enclosing prefix and cannot be extended".to_owned(),
            ParseErrorKind::UnterminatedAsmBody => "unterminated inline assembly body".to_owned(),
            ParseErrorKind::ChainedComparison => "comparison operators are non-associative".to_owned(),
            ParseErrorKind::ForeignConventionOnBinding => "a foreign binding cannot carry its own calling convention".to_owned(),
            ParseErrorKind::ForeignMutOnFunction => "a foreign function cannot be declared 'mut'".to_owned(),
            ParseErrorKind::NestedForeignBlock => "foreign blocks cannot nest".to_owned(),
        }
    }
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}
