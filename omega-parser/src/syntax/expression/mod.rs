pub mod codeblock;
pub mod string;

use crate::syntax::{
    ParseError, SyntaxParser,
    expression::{codeblock::CodeblockExpr, string::StringExpr},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    String(StringExpr),
    Codeblock(CodeblockExpr),
}

impl SyntaxParser for Expression {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        choice((
            StringExpr::parser().map(Expression::String),
            CodeblockExpr::parser().map(Expression::Codeblock),
        ))
        .padded()
    }
}
