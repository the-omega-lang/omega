pub mod codeblock;
pub mod function_call;
pub mod string;

use crate::{
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

impl SyntaxParser for Expression {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        choice((
            // TODO: Fix recursion and remove boxed()
            StringExpr::parser().map(Expression::String),
            CodeblockExpr::parser().map(Expression::Codeblock).boxed(),
            FunctionCallExpr::parser()
                .map(Expression::FunctionCall)
                .boxed(),
        ))
        .padded()
    }
}
