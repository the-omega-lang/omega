pub mod declaration;
pub mod extern_declaration;
pub mod function_definition;

use crate::{
    parser,
    prelude::Expression,
    syntax::{
        ParseError,
        expression::ExpressionNode,
        statement::{
            declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt,
            function_definition::FunctionDefinitionStmt,
        },
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

impl RootStatement {
    parser!(() => Self {
        let semicolon_statements = choice((
            DeclarationStmt::parser().map(RootStatement::Declaration),
            ExternDeclarationStmt::parser().map(RootStatement::ExternDeclaration),
        ))
        .then_ignore(just(';').padded());
        choice((
            semicolon_statements,
            FunctionDefinitionStmt::parser(recursive(|stmt_parser| {
                Statement::parser(ExpressionNode::parser(stmt_parser))
            }))
            .map(RootStatement::FunctionDefinition),
        ))
        .padded()
    });
}

// Function scope statements
#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    ExternDeclaration(ExternDeclarationStmt),
    Expression(ExpressionNode),
}

impl Statement {
    parser!((expr_parser => ExpressionNode) => Self {
        choice((
            DeclarationStmt::parser().map(Statement::Declaration),
            ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
            expr_parser.map(Statement::Expression),
        ))
        .then_ignore(just(';').padded())
        .padded()
    });
}
