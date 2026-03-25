pub mod string;

use crate::syntax::{ParseError, SyntaxParser, expression::string::StringExpr};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Expression {
    String(StringExpr),
}

impl SyntaxParser for Expression {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        choice((StringExpr::parser().map(Expression::String),))
    }
}
