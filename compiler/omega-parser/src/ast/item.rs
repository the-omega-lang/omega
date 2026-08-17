use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::{CodeblockExpr, ExpressionNode, MacroInvocationExpr};
use crate::ast::generics::GenericParam;
use crate::ast::identifier::{Ident, Path};
use crate::ast::self_mode::SelfMode;
use crate::ast::statement::{
    DeclarationStmt, ExternDeclarationStmt, FunctionDefinitionStmt, WalrusStmt,
};
use crate::ast::r#type::{Param, Type};
use crate::ast::visibility::Visibility;
use crate::diagnostics::Span;
use crate::lexer::Token;

// Top level/global scope statements
#[derive(Debug, Clone)]
pub enum Item {
    Declaration(DeclarationStmt),
    /// `ident : Type = value;` -- a typed top-level declaration with an
    /// initializer, the item-level counterpart of `Statement::
    /// DeclarationWithInit` (see its own doc comment for why this reuses
    /// `DeclarationStmt` rather than a dedicated struct). Unlike
    /// `Declaration` above, `value` is semantically restricted at
    /// analysis time to a compile-time-known expression, same as
    /// `Walrus`'s own value -- see `AnalysisErrorKind::TopLevelValueNotComp`.
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ExternDeclaration(ExternDeclarationStmt),
    FunctionDefinition(FunctionDefinitionStmt),
    Struct(StructStmt),
    /// Top-level only -- there is deliberately no `Statement::Struct` or
    /// `Statement::Enum`: both a struct's and an enum's identity (tag
    /// values, cross-module construction, cross-module caching) is
    /// inherently module-level, and statement position reports a dedicated
    /// parse error instead (see `ParseErrorKind::StructNotAllowedHere`/
    /// `EnumNotAllowedHere`).
    Enum(EnumStmt),
    /// See `UnionStmt`'s doc comment -- same top-level-only reasoning as
    /// `Struct`/`Enum` above.
    Union(UnionStmt),
    /// See `SpecStmt`'s doc comment -- same top-level-only reasoning as
    /// `Struct`/`Enum`/`Union` above.
    Spec(SpecStmt),
    Gap(GapStmt),
    Glue(GlueStmt),
    Conform(ConformStmt),
    Primitive(PrimitiveStmt),
    /// `[comp] ident := value;` -- top-level walrus, type always inferred
    /// from `value`. `comp` (see `WalrusStmt::comp`) decides whether this
    /// gets real storage or is substituted everywhere with no storage at
    /// all; either way `value` must be compile-time-known -- checked
    /// during analysis, not here (see
    /// `AnalysisErrorKind::TopLevelValueNotComp`), consistent with how
    /// this language generally defers semantic rules to analysis rather
    /// than the grammar wherever the shape alone doesn't already make
    /// something illegal.
    Walrus(WalrusStmt),
    Import(ImportStmt),
    /// Expanded away entirely (along with `MacroInvocation` below) by
    /// `omega_parser::macros::expand` before HIR lowering ever runs -- see
    /// `MacroDefinitionStmt`'s doc comment.
    MacroDefinition(MacroDefinitionStmt),
    /// `name$(arg, ...);` in item position; the expansion pass splices its
    /// expansion's items in place of this node.
    MacroInvocation(MacroInvocationExpr),
}

#[derive(Debug, Clone)]
pub struct ItemNode {
    pub item: Item,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructStmt {
    /// `@packing(...)`/`@suppress(...)` written directly above the `struct`
    /// keyword -- see `omega_analyzer::annotations`.
    pub annotations: Vec<AnnotationNode>,
    /// `exposed`/`internal`/(default `Hidden`), written directly before
    /// the `struct` keyword -- see `visibility::Visibility`.
    pub visibility: Visibility,
    pub ident: Ident,
    /// `<T, U, ...>` immediately after `ident` -- empty for an ordinary,
    /// non-generic struct. See `Type::Generic`'s doc comment for how these
    /// names are referenced at a use site.
    pub generics: Vec<GenericParam>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
    /// `true` for a `marker` declaration -- grammatically always paired
    /// with an empty `fields` (the `marker` parse path never reaches the
    /// field-list loop at all, see `parser::item::parse_struct_def`), so a
    /// marker's "no data, ever" property is structural, not merely a
    /// zero-length list an ordinary struct could also happen to have.
    /// Everything else (generics and functions) is identical
    /// to an ordinary struct's -- see `ResolvedStructType`'s own doc
    /// comment for why this reuses `Struct` wholesale instead of being a
    /// separate item kind.
    pub is_marker: bool,
}

/// A C/Rust-style union: every field overlaps the same storage (no tag, no
/// proof) -- see `StructStmt`'s doc comment for why the shape mirrors it
/// exactly rather than sharing a type; unions are deliberately their own
/// parallel item pipeline, same precedent as `enum` alongside `struct`.
#[derive(Debug, Clone)]
pub struct UnionStmt {
    /// See `StructStmt::annotations`'s doc comment. `@packing` isn't
    /// recognized on a union yet (only asked for on structs/enums) --
    /// `@suppress` is.
    pub annotations: Vec<AnnotationNode>,
    /// See `StructStmt::visibility`'s doc comment.
    pub visibility: Visibility,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

/// An omega-style enum:
///
/// ```text
/// enum Name<T, ...>(tag: i16, description: *u8) {
///     Bad(-1, "..."),
///     First(0, "...") { message: *u8; },
///     Second(1, "...");
///
///     print_description(self) => void { ... }
/// }
/// ```
///
/// Four orthogonal pieces per the language design:
/// - a *header* (the parenthesized list): fields present on **every**
///   variant, whose values are per-variant constants supplied in each
///   variant's `(...)` -- Java-style "each variant calls the constructor
///   with pre-specified data". The header may start with the special entry
///   `tag: <int type>`, making the tag explicit; otherwise the tag is an
///   implicit auto-incrementing `u16`. Whether the first entry *is* the tag
///   is decided by semantic analysis (it's just a field named `tag` here) --
///   the parser records the raw list.
/// - *shared dynamic fields* (an optional `field: Type;` list right after
///   the opening `{`, before the first variant): also present on **every**
///   variant like the header, but -- unlike the header -- runtime-valued,
///   not a per-variant constant: every construction site supplies them in
///   its body literal (see `EnumVariantStmt`), and they're freely
///   assignable afterward, exactly like a body field.
/// - *variants*, each optionally with a `{ field: Type; ... }` body of
///   variant-specific fields -- at runtime the enum's body region is a
///   union of all variant bodies, but the language only ever lets you touch
///   the body of the variant you provably have.
/// - *functions*, after a `;` terminating the variant list (Java-style) --
///   ordinary struct-style functions (`self` = member, no `self` = static).
#[derive(Debug, Clone)]
pub struct EnumStmt {
    /// See `StructStmt::annotations`'s doc comment.
    pub annotations: Vec<AnnotationNode>,
    /// `exposed`/`internal`/(default `Hidden`) on the enum itself -- every
    /// variant always inherits this exact value (there is no per-variant
    /// modifier, enforced structurally: `parse_enum_variant` never offers a
    /// visibility-prefix parse position for a variant name).
    pub visibility: Visibility,
    pub ident: Ident,
    /// `<T, U, ...>` -- empty for an ordinary, non-generic enum; same
    /// use-site rules as `StructStmt::generics`.
    pub generics: Vec<GenericParam>,
    pub header: Vec<EnumHeaderField>,
    /// The optional shared-dynamic-fields section -- empty when the enum
    /// declares none. Plain `DeclarationStmt`s, same as a struct field or a
    /// variant's own body field (no position-sensitive rules like the
    /// header's `tag` has, so no dedicated span-carrying type is needed).
    pub dynamic_fields: Vec<DeclarationStmt>,
    pub variants: Vec<EnumVariantStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

/// One header entry (`name: Type`) -- unlike a struct field's plain
/// `DeclarationStmt`, this keeps its own span: header entries have
/// position-sensitive rules (`tag` must come first) that deserve an error
/// pointing at the exact entry, not the whole enum.
#[derive(Debug, Clone)]
pub struct EnumHeaderField {
    pub ident: Ident,
    /// The declared name only -- see `DeclarationStmt::name_span`.
    pub name_span: Span,
    pub r#type: Type,
    pub visibility: Visibility,
    pub span: Span,
}

/// One variant: `Name`, `Name(args...)`, `Name { fields... }`, or
/// `Name(args...) { fields... }`. `span` covers the variant's name --
/// where identity-level problems (duplicate name, duplicate tag, wrong
/// argument count) are anchored; per-value problems anchor at the
/// argument expressions' own spans.
#[derive(Debug, Clone)]
pub struct EnumVariantStmt {
    pub ident: Ident,
    pub span: Span,
    /// The header values (the explicit tag first, if the enum declares
    /// one) -- constant expressions, enforced during analysis.
    pub args: Vec<ExpressionNode>,
    /// The variant's own body fields -- empty for a body-less variant.
    pub fields: Vec<DeclarationStmt>,
}

/// A `spec` -- a function-only interface/trait, in one of two surface
/// forms:
///
/// ```text
/// spec Name<T, ...> {
///     required(self) => T;
///     with_default(self) => T { self.required() }
/// }
///
/// spec Alias<T, ...> = Member1 + Member2;
/// ```
///
/// The declaration form (`{...}` body) lists the spec's own function
/// members, each either *required* (no body -- every implementor must
/// provide one) or *default* (a body; overridable per implementor). A spec
/// declares nothing else: what a default body may call on `self` is exactly
/// this spec's own requirements and defaults, unless a `conform` block's
/// own bounds put more in scope.
///
/// The alias form (`=`, `+`-separated, no body) is pure conjunction sugar
/// for "requires all of these" with no functions of its own -- `functions`
/// is always empty for an alias. An alias is a *name*, never a contract:
/// it is not itself conformable (see
/// `AnalysisErrorKind::ConformToAliasSpec`), it is satisfied by conforming
/// each member separately. See `parser::item::parse_spec_def`.
#[derive(Debug, Clone)]
pub struct SpecStmt {
    pub ident: Ident,
    /// `exposed`/`internal`/(default `Hidden`).
    pub visibility: Visibility,
    pub generics: Vec<GenericParam>,
    /// The alias form's member list (`spec Alias = A + B;`), carried in
    /// this same field. Always empty for the declaration form -- a spec
    /// declaration has no dependencies, and never did beyond the removed
    /// provisioning form. See `is_alias`.
    pub dependencies: Vec<Type>,
    pub functions: Vec<SpecFunctionStmt>,
    /// `true` for the `=`/`+`-separated alias form (`spec Alias = A + B;`),
    /// `false` for the ordinary `{}` declaration form -- both are carried in
    /// this same struct shape (see the type's own doc comment), so this is
    /// the one thing that actually tells them apart. An alias has no
    /// function list of its own; what it means for a bound is resolved
    /// during analysis (`Analyzer::flatten_spec`), not here.
    pub is_alias: bool,
    /// `@suppress` -- the only annotation a spec accepts (see
    /// `omega_analyzer::annotations::ItemKind::Spec`); validated during
    /// analysis, not parsing.
    pub annotations: Vec<AnnotationNode>,
}

/// One function member of a spec -- `body: None` for a required function
/// (every implementor must provide a matching method, own or default),
/// `body: Some` for a default (used as-is unless a concrete implementor
/// overrides it with its own same-named, same-signature method). `Self` is
/// meaningful inside `params`/`return_type`/`body` here -- see
/// `omega_hir::lower::lower_function_def`'s spec-aware `self`-typing case.
#[derive(Debug, Clone)]
pub struct SpecFunctionStmt {
    pub ident: Ident,
    /// The function name only. See `FunctionDefinitionStmt::name_span` --
    /// a spec/gap function is never wrapped in an `ItemNode` of its own
    /// either, so without these it would inherit the enclosing `spec`'s
    /// span for every diagnostic anchored on it.
    pub name_span: Span,
    /// From the name through the declared return type, excluding the body.
    pub signature_span: Span,
    /// The declared return type only.
    pub return_type_span: Span,
    /// See `FunctionDefinitionStmt::self_mode`. Always `*self`/`*mut self`
    /// (`SelfMode::Pointer`/`MutPointer`) for an ordinary spec function --
    /// by-value self is rejected during spec signature resolution (see
    /// `Analyzer::resolve_spec_functions`), since it can't survive `spec
    /// *T` dynamic dispatch's `Self`-erasure.
    pub self_mode: Option<SelfMode>,
    pub params: Vec<Param>,
    /// A final `...`, matching ordinary function-type variadics.
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<CodeblockExpr>,
}

/// `gap Name { function(params) => Return; ... }` -- a named, global
/// platform capability signature. A gap has no visibility, generic, spec, or
/// implementation shape: its functions are declarations only.
#[derive(Debug, Clone)]
pub struct GapStmt {
    pub ident: Ident,
    pub functions: Vec<SpecFunctionStmt>,
}

/// `glue qualified::Gap { function(params) => Return { ... } }` -- the one
/// concrete implementation of a named gap. It has no name or visibility of
/// its own; the target gap supplies the linker namespace.
#[derive(Debug, Clone)]
pub struct GlueStmt {
    pub gap: Path,
    pub functions: Vec<FunctionDefinitionStmt>,
}

/// A nominal conformance declaration: `conform<T> Target<T> to Spec<T> { ... }`.
/// The block itself is unnamed; member visibility comes from the matched spec
/// requirement rather than surface modifiers in this list.
#[derive(Debug, Clone)]
pub struct ConformStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub spec: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}

/// Inherent methods for a compiler-provided type:
/// `primitive<T> []T { exposed method(*self) => ... }`. Target and package
/// restrictions are semantic, not parser concerns.
#[derive(Debug, Clone)]
pub struct PrimitiveStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}

/// `import a::b::c;` -- root-level only (like `extern`/`struct`), never
/// inside a function body: nothing asks for that, and it's easy to add
/// later if it ever comes up. Whether `path` names a whole module or an item
/// inside one isn't decidable from syntax alone (`import a::b::c;` is
/// identical text for both) -- that's resolved later, once the module tree
/// is known, by `omega_analyzer::resolver::ModuleResolver` (implemented by
/// `omega-driver`). The parser only knows this is a path to *something*.
#[derive(Debug, Clone)]
pub struct ImportStmt {
    /// `@suppress(...)` written directly above `import` -- the only
    /// annotation an import accepts (see `omega_analyzer::annotations::
    /// ItemKind::Import`); anything else is rejected the ordinary
    /// `AnnotationNotApplicable` way.
    pub annotations: Vec<AnnotationNode>,
    /// `import reveal path;` -- bypasses the visibility check on whatever
    /// `path` resolves to, for *this importing module's own* later
    /// references through the resulting alias (does not make the alias
    /// itself visible to any third module -- there is no re-export concept
    /// in this language). See `omega_analyzer::analysis::Analyzer::
    /// reveal_stack`'s doc comment for the general `reveal` mechanism this
    /// plugs into.
    pub reveal: bool,
    pub root: ImportRoot,
    pub path: Path,
}

/// Where an `import`'s `path` is anchored -- the leading `root::`/`extern::`
/// the parser peeked for before parsing `path` itself (see
/// `parser::item::parse_item`'s `TokenKind::Import` arm). Purely syntactic;
/// turning this into an actual absolute module path is
/// `omega_driver`'s module-path arithmetic's job, once the module tree
/// is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRoot {
    /// The default: resolved relative to the *importing* module's own
    /// directory (a directory-shaped module's own directory is itself; a
    /// leaf file's is its parent -- see `Driver::relative_base`).
    Local,
    /// `root::...` -- always resolved from the current project's own root,
    /// regardless of how deeply nested the importing module is.
    ProjectRoot,
    /// `extern::name::...` -- resolved from the external project registered
    /// as `name` (via `--extern=name:path`) instead of the local project's
    /// own root; `path.head` is that name, by convention also that
    /// project's own top-level module segment.
    Extern,
}

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
