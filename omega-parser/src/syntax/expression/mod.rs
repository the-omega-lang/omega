pub mod codeblock;
pub mod function_call;
pub mod string;

use crate::{
    parser,
    prelude::Statement,
    syntax::{
        ParseError,
        expression::{
            codeblock::CodeblockExpr, function_call::FunctionCallExpr, string::StringExpr,
        },
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    String(StringExpr),
    Codeblock(CodeblockExpr),
    FunctionCall(FunctionCallExpr),
}

impl Expression {
    parser!((stmt_parser => Statement) => Self {
        recursive(|expr_parser| {
            choice((
                CodeblockExpr::parser(stmt_parser).map(Expression::Codeblock),
                FunctionCallExpr::parser(expr_parser)
                    .map(Expression::FunctionCall),
                StringExpr::parser().map(Expression::String),
            ))
        })
        .padded()
    });
}
