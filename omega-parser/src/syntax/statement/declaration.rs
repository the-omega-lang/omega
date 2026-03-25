use crate::syntax::{ParseError, SyntaxParser, identifier::Ident, r#type::Type};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}

impl SyntaxParser for DeclarationStmt {
    fn parser<'a>() -> impl chumsky::Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        Ident::parser()
            .padded()
            .then_ignore(just(':').padded())
            .then(Type::parser())
            .map(|(ident, typ)| Self { ident, r#type: typ })
    }
}
