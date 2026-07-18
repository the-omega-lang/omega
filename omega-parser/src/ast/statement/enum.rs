use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::ExpressionNode;
use crate::ast::generics::GenericParam;
use crate::ast::identifier::Ident;
use crate::ast::statement::{declaration::DeclarationStmt, function_definition::FunctionDefinitionStmt};
use crate::ast::r#type::Type;
use crate::diagnostics::Span;

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
    pub ident: Ident,
    /// `<T, U, ...>` -- empty for an ordinary, non-generic enum; same
    /// use-site rules as `StructStmt::generics`.
    pub generics: Vec<GenericParam>,
    /// See `StructStmt::implements`'s doc comment -- same rules.
    pub implements: Vec<Type>,
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
    pub r#type: Type,
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
