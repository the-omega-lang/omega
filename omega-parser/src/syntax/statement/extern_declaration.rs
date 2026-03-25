use crate::syntax::{
    ParseError, SyntaxParser, identifier::Ident, statement::declaration::Declaration, r#type::Type,
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct ExternDeclaration {
    pub ident: Ident,
    pub r#type: Type,
}

impl SyntaxParser for ExternDeclaration {
    fn parser<'a>() -> impl chumsky::Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        just("extern")
            .padded()
            .ignore_then(Declaration::parser())
            .map(|decl| Self {
                ident: decl.ident,
                r#type: decl.r#type,
            })
    }
}
