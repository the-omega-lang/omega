use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}
