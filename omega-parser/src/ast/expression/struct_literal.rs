use crate::ast::expression::ExpressionNode;
use crate::ast::identifier::{Ident, Path};
use crate::diagnostics::Span;

/// `Name { field: value; ... }` -- builds a whole struct value in one
/// expression, one initializer per field (analysis requires *every* field to
/// be covered exactly once). Field initializers are `;`-terminated, matching
/// the struct definition syntax they mirror, not comma-separated.
///
/// `path` is the struct's (possibly module-qualified) type name -- kept as a
/// raw `Path` like every other name at this layer; whether it actually names
/// a struct is analysis's question. Explicit generic arguments
/// (`List<u32> { ... }`) are not part of this grammar: `<` after a path in
/// expression position already means comparison, the same ambiguity that
/// leads Rust to require a turbofish there.
#[derive(Debug, Clone)]
pub struct StructLiteralExpr {
    pub path: Path,
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
