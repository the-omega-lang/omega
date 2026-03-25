pub mod declaration;
pub mod extern_declaration;

use crate::syntax::{
    ParseError, SyntaxParser,
    statement::{declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    ExternDeclaration(ExternDeclarationStmt),
}

impl SyntaxParser for Statement {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        choice((
            DeclarationStmt::parser().map(Statement::Declaration),
            ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
        ))
        .then_ignore(just(';').padded())
    }
}
