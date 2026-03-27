use crate::{parser, prelude::Statement, syntax::ParseError};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct CodeblockExpr(pub Vec<Statement>);

impl CodeblockExpr {
    parser!((stmt_parser => Statement) => Self {
        just('{')
            .padded()
            .ignore_then(stmt_parser.repeated().collect::<Vec<_>>())
            .map(|stmts| CodeblockExpr(stmts))
            .then_ignore(just('}').padded())
    });
}
