pub mod assignment;
pub mod codeblock;
pub mod function_call;
pub mod number;
pub mod string;

use crate::{
    next_node_id, parser,
    prelude::{Ident, Statement},
    syntax::{
        ParseError,
        expression::{
            assignment::AssignmentExpr, codeblock::CodeblockExpr, function_call::FunctionCallExpr,
            number::NumberExpr, string::StringExpr,
        },
        place::PlaceNode,
        statement::StatementNode,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    Place(Box<PlaceNode>),
    Number(NumberExpr),
    String(StringExpr),
    Codeblock(CodeblockExpr),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
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
                FunctionCallExpr::parser(expr_parser.clone())
                    .map(Expression::FunctionCall),
                AssignmentExpr::parser(expr_parser.clone()).map(|x| Expression::Assignment(Box::new(x))),
                NumberExpr::parser().map(Expression::Number),
                StringExpr::parser().map(Expression::String),
                PlaceNode::parser(expr_parser.clone()).map(|x| Expression::Place(Box::new(x))),
            )).map_with(|expression, extra| ExpressionNode {
                id: next_node_id(), expression, span: extra.span()
            })
        })
        .padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|expr_parser| Self::parser(StatementNode::parser(expr_parser)))
    }
}
