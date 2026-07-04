pub mod assignment;
pub mod codeblock;
pub mod field_access;
pub mod function_call;
pub mod index;
pub mod number;
pub mod string;

use crate::{
    parser,
    prelude::Ident,
    syntax::{
        ParseError,
        expression::{
            assignment::{AssignmentExpr, AssignmentPostfix},
            codeblock::CodeblockExpr,
            field_access::{FieldAccessExpr, FieldAccessPostfix},
            function_call::{FunctionCallExpr, FunctionCallPostfix},
            index::{IndexExpr, IndexPostfix},
            number::NumberExpr,
            string::StringExpr,
        },
        statement::StatementNode,
    },
};
use chumsky::prelude::*;

/// The parser only knows syntax, not semantics: `FieldAccess`/`Index` are
/// just expression-forming operators here, the same as `FunctionCall`. There
/// is no "place"/lvalue concept at this layer -- deciding which expression
/// shapes denote an addressable location is HIR lowering's job.
#[derive(Debug, Clone)]
pub enum Expression {
    Ident(Ident),
    FieldAccess(Box<FieldAccessExpr>),
    Index(Box<IndexExpr>),
    Number(NumberExpr),
    String(StringExpr),
    Codeblock(CodeblockExpr),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
}

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub expression: Expression,
    pub span: SimpleSpan,
}

pub enum Postfix {
    Call(FunctionCallPostfix),
    FieldAccess(FieldAccessPostfix),
    Index(IndexPostfix),
    Assignment(AssignmentPostfix),
}

impl Postfix {
    fn into_expression(self, expr: ExpressionNode) -> Expression {
        match self {
            Self::Call(x) => Expression::FunctionCall(FunctionCallExpr {
                callee: Box::new(expr),
                args: x.args,
            }),
            Self::FieldAccess(x) => Expression::FieldAccess(Box::new(FieldAccessExpr {
                base: expr,
                field: x.field,
            })),
            Self::Index(x) => Expression::Index(Box::new(IndexExpr {
                base: expr,
                index: x.index,
            })),
            Self::Assignment(x) => Expression::Assignment(Box::new(AssignmentExpr {
                target: expr,
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
                expression, span: extra.span()
            });

            let postfix = choice((
                FunctionCallPostfix::parser(expr_parser.clone()).map(Postfix::Call),
                FieldAccessPostfix::parser().map(Postfix::FieldAccess),
                IndexPostfix::parser(expr_parser.clone()).map(Postfix::Index),
                AssignmentPostfix::parser(expr_parser.clone()).map(Postfix::Assignment)
            ));

            primary
                .then(postfix.repeated().collect())
                .map_with(|(expr, postfixes): (ExpressionNode, Vec<Postfix>), extra| {
                    let mut expression = expr;
                    for postfix in postfixes {
                        let expr = postfix.into_expression(expression);
                        expression = ExpressionNode {
                            expression: expr, span: extra.span()
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
