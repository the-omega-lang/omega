use crate::{
    prelude::Statement,
    syntax::{ParseError, SyntaxParser, identifier::Ident, r#type::Type},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct CodeblockExpr(pub Vec<Statement>);

impl SyntaxParser for CodeblockExpr {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        just('{')
            .padded()
            .ignore_then(Statement::parser().repeated().collect::<Vec<_>>())
            .map(|stmts| CodeblockExpr(stmts))
            .then_ignore(just('}').padded())
    }
}
