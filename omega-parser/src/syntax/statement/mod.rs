pub mod declaration;
pub mod extern_declaration;
pub mod for_stmt;
pub mod function_definition;
pub mod r#return;
pub mod r#struct;
pub mod walrus;
pub mod while_stmt;

use crate::{
    parser,
    prelude::{IfExpr, StructStmt},
    syntax::{
        ParseError,
        expression::{Expression, ExpressionNode},
        statement::{
            declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt,
            for_stmt::ForStmt, function_definition::FunctionDefinitionStmt, r#return::ReturnStmt,
            walrus::WalrusStmt, while_stmt::WhileStmt,
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
        let (expr_parser, stmt_parser) = StatementNode::configured_parsers();
        let function_def_parser = FunctionDefinitionStmt::parser(expr_parser, stmt_parser);
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
    /// `ident : type = value;` -- unlike `Walrus`, the type is written down
    /// explicitly, so lowering can desugar this straight into a plain
    /// `Declaration` + assignment pair itself (see `lower_stmt`), with no
    /// need for semantic analysis to infer anything first.
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ExternDeclaration(ExternDeclarationStmt),
    Expression(ExpressionNode),
    Return(ReturnStmt),
    Struct(StructStmt),
    Walrus(WalrusStmt),
    While(WhileStmt),
    /// Boxed since `ForStmt.init` embeds a bare `Statement` -- without the
    /// indirection here, `Statement` would have infinite size.
    For(Box<ForStmt>),
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
                DeclarationStmt::parser()
                    .then(just('=').trivia_padded().ignore_then(expr_parser.clone()).or_not())
                    .map(|(decl, init)| match init {
                        Some(value) => Statement::DeclarationWithInit(decl, value),
                        None => Statement::Declaration(decl),
                    }),
                ExternDeclarationStmt::parser().map(Statement::ExternDeclaration),
                ReturnStmt::parser(expr_parser.clone()).map(Statement::Return),
                expr_parser.clone().map(Statement::Expression),
            ))
            .then_ignore(just(';').trivia_padded())
            .trivia_padded();

            // Block-terminated statements (end in `}`, not `;`) -- matching
            // the common C-family/Rust convention that a statement whose
            // last token is already a closing brace doesn't need a `;` of
            // its own. A trailing `;` is still tolerated (`.or_not()`)
            // rather than rejected, so a stray one out of habit still
            // parses.
            let terminal = choice((
                StructStmt::parser(DeclarationStmt::parser(), FunctionDefinitionStmt::parser(expr_parser.clone(), stmt_parser.clone())).map(Statement::Struct),
                IfExpr::parser(expr_parser.clone(), stmt_parser.clone())
                    .map_with(|if_expr, extra| ExpressionNode { expression: Expression::If(Box::new(if_expr)), span: extra.span() })
                    .map(Statement::Expression),
                WhileStmt::parser(expr_parser.clone(), stmt_parser.clone()).map(Statement::While),
                ForStmt::parser(expr_parser, stmt_parser).map(|f| Statement::For(Box::new(f))),
            ))
            .then_ignore(just(';').trivia_padded().or_not());

            choice((terminal, nonterminal))
                .map_with(|statement, extra| StatementNode { statement, span: extra.span() })
                .trivia_padded()
        })
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        Self::configured_parsers().1
    }

    /// Builds one shared mutually-recursive expression/statement parser
    /// graph and hands back *both* handles -- unlike `configured_parser`,
    /// which only exposes the statement side. Needed wherever something
    /// (currently just `FunctionDefinitionStmt`, for a function body's
    /// `CodeblockExpr`) has to embed both an expression parser and a
    /// statement parser that agree on the same grammar, without already
    /// being handed one from an enclosing `recursive` closure the way
    /// `StatementNode::parser`'s own body is.
    pub fn configured_parsers<'a>() -> (
        impl Parser<'a, &'a str, ExpressionNode, ParseError<'a>> + Clone,
        impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone,
    ) {
        let mut stmt_parser = Recursive::declare();
        let expr_parser = ExpressionNode::parser(stmt_parser.clone());
        stmt_parser.define(Self::parser(expr_parser.clone()));
        (expr_parser, stmt_parser)
    }
}
