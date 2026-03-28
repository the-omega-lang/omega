pub mod codeblock;
pub mod function_call;
pub mod string;

use crate::{
    next_node_id, parser,
    prelude::Statement,
    syntax::{
        ParseError,
        expression::{
            codeblock::CodeblockExpr, function_call::FunctionCallExpr, string::StringExpr,
        },
        statement::StatementNode,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    String(StringExpr),
    Codeblock(CodeblockExpr),
    FunctionCall(FunctionCallExpr),
}

pub type NodeId = u64;

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub id: NodeId,
    pub expression: Expression,
    pub span: SimpleSpan,
}

impl ExpressionNode {
    parser!((stmt_parser => StatementNode) => Self {
        recursive(|expr_parser| {
            choice((
                CodeblockExpr::parser(stmt_parser).map(Expression::Codeblock),
                FunctionCallExpr::parser(expr_parser)
                    .map(Expression::FunctionCall),
                StringExpr::parser().map(Expression::String),
            )).map_with(|expression, extra| ExpressionNode {
                id: next_node_id(), expression, span: extra.span()
            })
        })
        .padded()
    });
}
