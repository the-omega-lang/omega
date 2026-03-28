pub mod declaration;
pub mod extern_declaration;
pub mod function_definition;

use crate::{
    NodeId, next_node_id, parser,
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

#[derive(Debug, Clone)]
pub struct RootStatementNode {
    pub id: NodeId,
    pub root_stmt: RootStatement,
    pub span: SimpleSpan,
}

impl RootStatementNode {
    parser!(() => Self {
        let semicolon_statements = choice((
            DeclarationStmt::parser().map(RootStatement::Declaration),
            ExternDeclarationStmt::parser().map(RootStatement::ExternDeclaration),
        ))
        .then_ignore(just(';').padded());
        choice((
            semicolon_statements,
            FunctionDefinitionStmt::parser(recursive(|stmt_parser| {
                StatementNode::parser(ExpressionNode::parser(stmt_parser))
            })).map(RootStatement::FunctionDefinition),
        ))
        .map_with(|root_stmt, extra| RootStatementNode {
            id: next_node_id(),
            root_stmt, span:
            extra.span()
        })
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

#[derive(Debug, Clone)]
pub struct StatementNode {
    pub id: NodeId,
    pub statement: Statement,
    pub span: SimpleSpan,
}

impl StatementNode {
    parser!((expr_parser => ExpressionNode) => Self {
        choice((
            DeclarationStmt::parser().map(Statement::Declaration),
            ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
            expr_parser.map(Statement::Expression),
        )).map_with(|statement, extra| StatementNode { id: next_node_id(), statement, span: extra.span() })
        .then_ignore(just(';').padded())
        .padded()
    });
}
