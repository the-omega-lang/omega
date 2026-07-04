pub mod declaration;
pub mod extern_declaration;
pub mod function_definition;
pub mod r#return;
pub mod r#struct;
pub mod walrus;

use crate::{
    parser,
    prelude::StructStmt,
    syntax::{
        ParseError,
        expression::ExpressionNode,
        statement::{
            declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt,
            function_definition::FunctionDefinitionStmt, r#return::ReturnStmt, walrus::WalrusStmt,
        },
    },
};
use crate::syntax::trivia::TriviaExt;
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
    pub root_stmt: RootStatement,
    pub span: SimpleSpan,
}

impl RootStatementNode {
    parser!(() => Self {
        let semicolon_statements = choice((
            DeclarationStmt::parser().map(RootStatement::Declaration),
            ExternDeclarationStmt::parser().map(RootStatement::ExternDeclaration),
        ))
        .then_ignore(just(';').trivia_padded());
        let function_def_parser = FunctionDefinitionStmt::parser(StatementNode::configured_parser());
        choice((
            semicolon_statements,
            function_def_parser.clone()
                .map(RootStatement::FunctionDefinition),
            StructStmt::parser(DeclarationStmt::parser(), function_def_parser).map(RootStatement::Struct),
        ))
        .map_with(|root_stmt, extra| RootStatementNode {
            root_stmt, span:
            extra.span()
        })
        .trivia_padded()
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
    Walrus(WalrusStmt),
}

#[derive(Debug, Clone)]
pub struct StatementNode {
    pub statement: Statement,
    pub span: SimpleSpan,
}

impl StatementNode {
    parser!((expr_parser => ExpressionNode) => Self {
        recursive(|stmt_parser| {
            let nonterminal = choice((
                // Non-terminal statements
                // `WalrusStmt` before `DeclarationStmt`: both start with an
                // identifier, and putting the longer/more-specific `:=`
                // token first avoids relying on `choice`'s backtracking to
                // recover from `DeclarationStmt` matching `:` and then
                // failing to parse a `Type` starting at `= ...`.
                WalrusStmt::parser(expr_parser.clone()).map(Statement::Walrus),
                DeclarationStmt::parser().map(Statement::Declaration),
                ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
                ReturnStmt::parser(expr_parser.clone()).map(Statement::Return),
                expr_parser.map(Statement::Expression), // TODO: Move expression to terminal in order to handle codeblocks
            ))
            .then_ignore(just(';').trivia_padded())
            .trivia_padded();

            let terminal = choice((
                // Terminal statements
                StructStmt::parser(DeclarationStmt::parser(), FunctionDefinitionStmt::parser(stmt_parser)).map(Statement::Struct),
            ));

            choice((terminal, nonterminal))
                .map_with(|statement, extra| StatementNode { statement, span: extra.span() })
                .trivia_padded()
        })
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|stmt_parser| Self::parser(ExpressionNode::parser(stmt_parser)))
    }
}
