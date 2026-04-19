pub mod declaration;
pub mod extern_declaration;
pub mod function_definition;
pub mod r#return;
pub mod r#struct;

use crate::{
    NodeId, next_node_id, parser,
    prelude::{Expression, StructStmt},
    syntax::{
        ParseError,
        expression::ExpressionNode,
        statement::{
            declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt,
            function_definition::FunctionDefinitionStmt, r#return::ReturnStmt,
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
    Struct(StructStmt),
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
            FunctionDefinitionStmt::parser(StatementNode::configured_parser())
                .map(RootStatement::FunctionDefinition),
            StructStmt::parser(DeclarationStmt::parser()).map(RootStatement::Struct),
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
    Return(ReturnStmt),
    Struct(StructStmt),
}

#[derive(Debug, Clone)]
pub struct StatementNode {
    pub id: NodeId,
    pub statement: Statement,
    pub span: SimpleSpan,
}

impl StatementNode {
    parser!((expr_parser => ExpressionNode) => Self {
        let nonterminal = choice((
            // Non-terminal statements
            DeclarationStmt::parser().map(Statement::Declaration),
            ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
            ReturnStmt::parser(expr_parser.clone()).map(Statement::Return),
            expr_parser.map(Statement::Expression), // TODO: Move expression to terminal in order to handle codeblocks
        ))
        .then_ignore(just(';').padded())
        .padded();

        let terminal = choice((
            // Terminal statements
            StructStmt::parser(DeclarationStmt::parser()).map(Statement::Struct),
        ));

        choice((terminal, nonterminal))
            .map_with(|statement, extra| StatementNode { id: next_node_id(), statement, span: extra.span() })
            .padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|stmt_parser| Self::parser(ExpressionNode::parser(stmt_parser)))
    }
}
