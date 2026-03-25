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

// Top level/global scope statements
#[derive(Debug, Clone)]
pub enum RootStatement {
    Declaration(DeclarationStmt),
    ExternDeclaration(ExternDeclarationStmt),
    FunctionDefinition(FunctionDefinitionStmt),
}

#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    ExternDeclaration(ExternDeclarationStmt),
}

impl SyntaxParser for RootStatement {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        let semicolon_statements = choice((
            DeclarationStmt::parser().map(RootStatement::Declaration),
            ExternDeclarationStmt::parser().map(RootStatement::ExternDeclaration),
        ))
        .then_ignore(just(';').padded());
        choice((
            semicolon_statements,
            FunctionDefinitionStmt::parser().map(RootStatement::FunctionDefinition),
        ))
        .padded()
    }
}

impl SyntaxParser for Statement {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        choice((
            DeclarationStmt::parser().map(Statement::Declaration),
            ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
        ))
        .then_ignore(just(';').padded())
        .padded()
    }
}
