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

macro_rules! primary_expr_generator {
    ($stmt_parser:expr, $custom_expr_parser:expr) => {
        choice((
            CodeblockExpr::parser($stmt_parser).map(Expression::Codeblock),
            FunctionCallExpr::parser($custom_expr_parser.clone()).map(Expression::FunctionCall),
            AssignmentExpr::parser($custom_expr_parser.clone()).map(Expression::Assignment),
            Ident::parser().map(Expression::Ident),
            NumberExpr::parser().map(Expression::Number),
            StringExpr::parser().map(Expression::String),
        ))
        .map_with(|expression, extra| ExpressionNode {
            id: next_node_id(),
            expression,
            span: extra.span(),
        })
    };
}

macro_rules! lookahead_expr_generator {
    ($custom_expr_parser:expr) => {
        choice((LookaheadIndexExpr::parser($custom_expr_parser)
            .map(|lookahead| LookaheadExpression::Index(lookahead)),))
    };
}

macro_rules! primary_with_lookahead_expr_generator {
    ($primary_parser:expr, $lookahead_parser:expr) => {
        $primary_parser
            .then($lookahead_parser.or_not())
            .map(|(expr, extended_expr_opt)| match extended_expr_opt {
                Some(lookahead) => lookahead.into_expression_node(expr),
                None => expr,
            })
    };
}

impl ExpressionNode {
    parser!((stmt_parser => StatementNode) => Self {
        recursive(|expr_parser| {
            let primary = primary_expr_generator!(stmt_parser.clone(), expr_parser);

            let lookahead = lookahead_expr_generator!(primary.clone());

            let recursing_lookahead = recursive(|lookahead_expr| {
                let primary = primary_expr_generator!(stmt_parser.clone(), lookahead_expr.clone());
                let lookahead = lookahead_expr_generator!(lookahead_expr.clone());
                primary_with_lookahead_expr_generator!(primary, lookahead)
            });

            // Primary expressions dont have lookahead and inner expressions do have lookahead
            recursing_lookahead
        })
        .padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|expr_parser| Self::parser(StatementNode::parser(expr_parser)))
    }
}
