use crate::parser;
use crate::syntax::{
    ParseError, identifier::Ident, statement::declaration::DeclarationStmt, r#type::Type,
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}

impl ExternDeclarationStmt {
    parser!(() -> Self {
        text::ascii::keyword("extern")
            .padded()
            .ignore_then(DeclarationStmt::parser())
            .map(|decl| Self {
                ident: decl.ident,
                r#type: decl.r#type,
            })
    });
}
