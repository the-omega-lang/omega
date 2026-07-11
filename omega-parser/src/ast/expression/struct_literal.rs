use crate::ast::expression::ExpressionNode;
use crate::ast::identifier::{ExprPath, Ident};
use crate::diagnostics::Span;

/// `Name { field = value; ... }` -- builds a whole struct value (or, when the
/// path names an enum variant -- `Enum::Variant { ... }` -- an enum value) in
/// one expression, one initializer per field (analysis requires *every*
/// field to be covered exactly once). Field initializers are `;`-terminated,
/// matching the struct definition syntax they mirror, not comma-separated.
///
/// `path` is the built type's (possibly module-qualified, possibly
/// generic-argumented -- `List<u32> { ... }`, `Optional<u32>::Some { ... }`)
/// name -- kept raw like every other name at this layer; whether it actually
/// names a struct or an enum variant is analysis's question.
#[derive(Debug, Clone)]
pub struct StructLiteralExpr {
    pub path: ExprPath,
    pub fields: Vec<StructLiteralField>,
}

/// One `name: value;` initializer. `name_span` is the field name's own span,
/// so "no such field"/"field set twice" diagnostics can point at the name
/// itself rather than the whole literal.
#[derive(Debug, Clone)]
pub struct StructLiteralField {
    pub name: Ident,
    pub name_span: Span,
    pub value: ExpressionNode,
}
