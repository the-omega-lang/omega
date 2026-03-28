use crate::{
    parser,
    prelude::Statement,
    syntax::{ParseError, statement::StatementNode},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct CodeblockExpr(pub Vec<StatementNode>);

impl CodeblockExpr {
    parser!((stmt_parser => StatementNode) => Self {
        just('{')
            .padded()
            .ignore_then(stmt_parser.repeated().collect::<Vec<_>>())
            .map(|stmts| CodeblockExpr(stmts))
            .then_ignore(just('}').padded())
    });
}
