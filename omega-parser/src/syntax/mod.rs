pub mod identifier;
pub mod statement;
pub mod r#type;

use chumsky::{ParseResult, Parser, error::Rich};

pub type ParseError<'a> = chumsky::extra::Err<Rich<'a, char>>;

// All syntaxes, from most simple to most complex
// must implement the following trait
pub trait SyntaxParser
where
    Self: Sized,
{
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone;
    fn parse<'a>(input: &str) -> Result<Self, Vec<Rich<'_, char>>> {
        Self::parser().parse(input).into_result()
    }
}
