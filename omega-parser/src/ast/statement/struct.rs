use crate::ast::identifier::Ident;
use crate::ast::statement::{declaration::DeclarationStmt, function_definition::FunctionDefinitionStmt};

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub ident: Ident,
    /// `<T, U, ...>` immediately after `ident` -- empty for an ordinary,
    /// non-generic struct. See `Type::Generic`'s doc comment for how these
    /// names are referenced at a use site.
    pub generics: Vec<Ident>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}
