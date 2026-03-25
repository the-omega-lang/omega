use crate::syntax::{
    ParseError, SyntaxParser, identifier::Ident, statement::declaration::DeclarationStmt,
    r#type::Type,
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}

impl SyntaxParser for ExternDeclarationStmt {
    fn parser<'a>() -> impl chumsky::Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        text::ascii::keyword("extern")
            .padded()
            .ignore_then(DeclarationStmt::parser())
            .map(|decl| Self {
                ident: decl.ident,
                r#type: decl.r#type,
            })
    }
}
