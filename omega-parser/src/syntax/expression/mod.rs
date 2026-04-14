pub mod assignment;
pub mod codeblock;
pub mod function_call;
pub mod index;
pub mod number;
pub mod string;

use crate::{
    next_node_id, parser,
    prelude::{Ident, Statement},
    syntax::{
        ParseError,
        expression::{
            assignment::AssignmentExpr,
            codeblock::CodeblockExpr,
            function_call::FunctionCallExpr,
            index::{IndexExpr, LookaheadIndexExpr},
            number::NumberExpr,
            string::StringExpr,
        },
        statement::StatementNode,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    Ident(Ident),
    Number(NumberExpr),
    String(StringExpr),
    Codeblock(CodeblockExpr),
    FunctionCall(FunctionCallExpr),
    Assignment(AssignmentExpr),
    Index(Box<IndexExpr>),
}

#[derive(Debug, Clone)]
pub enum LookaheadExpression {
    Index(LookaheadIndexExpr),
}

impl LookaheadExpression {
    fn into_expression_node(self, expr: ExpressionNode) -> ExpressionNode {
        let id = expr.id;
        let mut span = expr.span;
        let extended_expr = match self {
            Self::Index(lookahead) => {
                span.end = lookahead.index.span.end;
                Expression::Index(Box::new(IndexExpr {
                    indexed: expr,
                    index: lookahead.index,
                }))
            }
        };
        ExpressionNode {
            id,
            expression: extended_expr,
            span,
        }
    }
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
                AssignmentExpr::parser(expr_parser.clone()).map(Expression::Assignment),
                Ident::parser().map(Expression::Ident),
                NumberExpr::parser().map(Expression::Number),
                StringExpr::parser().map(Expression::String)
            )).map_with(|expression, extra| ExpressionNode {
                id: next_node_id(), expression, span: extra.span()
            }).then(
                // Lookahead parsers
                choice((LookaheadIndexExpr::parser(expr_parser).map(|lookahead| {
                    LookaheadExpression::Index(lookahead)
                }),)).or_not()
            ).map(|(expr, extended_expr_opt)| {
                match extended_expr_opt {
                    Some(lookahead) => lookahead.into_expression_node(expr),
                    None => expr
                }
            })
        })
        .padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|expr_parser| Self::parser(StatementNode::parser(expr_parser)))
    }
}
