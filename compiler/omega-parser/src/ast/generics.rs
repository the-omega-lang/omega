use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub ident: Ident,
    pub bounds: Vec<Type>,
    pub default: Option<Type>,
}
