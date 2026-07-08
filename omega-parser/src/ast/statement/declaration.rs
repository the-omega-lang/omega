use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}
