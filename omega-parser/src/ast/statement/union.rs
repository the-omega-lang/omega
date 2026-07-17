use crate::ast::attribute::AttributeNode;
use crate::ast::generics::GenericParam;
use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;
use crate::ast::statement::{declaration::DeclarationStmt, function_definition::FunctionDefinitionStmt};

/// A C/Rust-style union: every field overlaps the same storage (no tag, no
/// proof) -- see `StructStmt`'s doc comment for why the shape mirrors it
/// exactly rather than sharing a type; unions are deliberately their own
/// parallel item pipeline, same precedent as `enum` alongside `struct`.
#[derive(Debug, Clone)]
pub struct UnionStmt {
    /// See `StructStmt::attributes`'s doc comment. `@packing` isn't
    /// recognized on a union yet (only asked for on structs/enums) --
    /// `@suppress` is.
    pub attributes: Vec<AttributeNode>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    /// See `StructStmt::implements`'s doc comment -- same rules.
    pub implements: Vec<Type>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}
