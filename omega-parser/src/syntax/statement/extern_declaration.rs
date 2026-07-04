use crate::parser;
use crate::syntax::{
    identifier::Ident, statement::declaration::DeclarationStmt, r#type::Type,
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}

impl ExternDeclarationStmt {
    parser!(() => Self {
        text::ascii::keyword("extern")
            .trivia_padded()
            .ignore_then(DeclarationStmt::parser())
            .map(|decl| Self {
                ident: decl.ident,
                r#type: decl.r#type,
            })
    });
}
