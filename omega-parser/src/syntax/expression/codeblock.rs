use crate::{
    parser,
    syntax::statement::StatementNode,
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct CodeblockExpr(pub Vec<StatementNode>);

impl CodeblockExpr {
    parser!((stmt_parser => StatementNode) => Self {
        just('{')
            .trivia_padded()
            .ignore_then(stmt_parser.repeated().collect::<Vec<_>>())
            .map(|stmts| CodeblockExpr(stmts))
            .then_ignore(just('}').trivia_padded())
    });
}
