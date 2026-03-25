pub mod declaration;
pub mod extern_declaration;
pub mod function_definition;

use crate::syntax::{
    ParseError, SyntaxParser,
    statement::{
        declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt,
        function_definition::FunctionDefinitionStmt,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    ExternDeclaration(ExternDeclarationStmt),
    FunctionDefinition(FunctionDefinitionStmt),
}

impl SyntaxParser for Statement {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        choice((
            DeclarationStmt::parser().map(Statement::Declaration),
            ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
            FunctionDefinitionStmt::parser().map(Statement::FunctionDefinition),
        ))
        .padded()
    }
}
