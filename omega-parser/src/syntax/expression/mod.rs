pub mod codeblock;
pub mod function_call;
pub mod string;

use std::rc::Rc;

use crate::{
    parser,
    prelude::Statement,
    syntax::{
        ParseError, SyntaxParser,
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
    parser!((stmt_parser => Statement) -> Self {
        recursive(|expr_parser| {
            choice((
                StringExpr::parser().map(Expression::String),
                CodeblockExpr::parser(stmt_parser.clone()).map(Expression::Codeblock),
                FunctionCallExpr::parser(expr_parser)
                    .map(Expression::FunctionCall),
            ))
        })
        .padded()
    });
}
