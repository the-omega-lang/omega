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
            assignment::{AssignmentExpr, AssignmentPostfix},
            codeblock::CodeblockExpr,
            function_call::{FunctionCallExpr, FunctionCallPostfix},
            number::NumberExpr,
            string::StringExpr,
        },
        place::{PlaceExpr, PlaceModifierPostfix},
        statement::StatementNode,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    Ident(Ident), // Only used for places
    Place(Box<PlaceExpr>),
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

pub enum Postfix {
    Call(FunctionCallPostfix),
    Place(PlaceModifierPostfix),
    Assignment(AssignmentPostfix),
}

impl Postfix {
    fn into_expression(self, expr: ExpressionNode) -> Expression {
        match self {
            Self::Call(x) => Expression::FunctionCall(FunctionCallExpr {
                callee: Box::new(expr),
                args: x.args,
            }),
            Self::Place(x) => Expression::Place(Box::new(PlaceExpr {
                base: expr,
                modifier: x,
            })),
            Self::Assignment(x) => Expression::Assignment(Box::new(AssignmentExpr {
                place: expr,
                value: Box::new(x.value),
            })),
        }
    }
}

impl ExpressionNode {
    parser!((stmt_parser => StatementNode) => Self {
        recursive(|expr_parser| {
            let primary = choice((
                CodeblockExpr::parser(stmt_parser).map(Expression::Codeblock),
                NumberExpr::parser().map(Expression::Number),
                StringExpr::parser().map(Expression::String),
                Ident::parser().map(Expression::Ident)
            )).map_with(|expression, extra| ExpressionNode {
                id: next_node_id(), expression, span: extra.span()
            });

            let postfix = choice((
                FunctionCallPostfix::parser(expr_parser.clone()).map(Postfix::Call),
                PlaceModifierPostfix::parser(expr_parser.clone()).map(Postfix::Place),
                AssignmentPostfix::parser(expr_parser.clone()).map(Postfix::Assignment)
            ));

            primary
                .then(postfix.repeated().collect())
                .map_with(|(expr, postfixes): (ExpressionNode, Vec<Postfix>), extra| {
                    let mut expression = expr;
                    for postfix in postfixes {
                        let expr = postfix.into_expression(expression);
                        expression = ExpressionNode {
                            id: next_node_id(), expression: expr, span: extra.span()
                        }
                    }

                    expression
                })
        })
        .padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|expr_parser| Self::parser(StatementNode::parser(expr_parser)))
    }
}
