use crate::parser;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `true`/`false` -- a bare keyword literal. `text::keyword` requires a word
/// boundary after the match (so `truest` doesn't parse as `true` followed by
/// a stray `st`), and this is tried before the general `Ident` parser in
/// `ExpressionNode`'s `primary` choice so the keywords aren't instead parsed
/// as (undefined) variable references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoolExpr(pub bool);

impl BoolExpr {
    parser!(() => Self {
        choice((
            text::keyword("true").to(true),
            text::keyword("false").to(false),
        ))
        .trivia_padded()
        .map(Self)
    });
}
