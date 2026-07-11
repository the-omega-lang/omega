use crate::ast::identifier::Ident;
use crate::ast::statement::{declaration::DeclarationStmt, function_definition::FunctionDefinitionStmt};

/// A C/Rust-style union: every field overlaps the same storage (no tag, no
/// proof) -- see `StructStmt`'s doc comment for why the shape mirrors it
/// exactly rather than sharing a type; unions are deliberately their own
/// parallel item pipeline, same precedent as `enum` alongside `struct`.
#[derive(Debug, Clone)]
pub struct UnionStmt {
    pub ident: Ident,
    pub generics: Vec<Ident>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}
