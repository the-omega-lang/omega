use crate::{
    parser,
    syntax::{ParseError, identifier::Ident, r#type::Type},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}

impl DeclarationStmt {
    parser!(() => Self {
        Ident::parser()
            .padded()
            .then_ignore(just(':').padded())
            .then(Type::parser())
            .map(|(ident, typ)| Self { ident, r#type: typ })
    });
}
