use crate::parser;
use crate::syntax::escape::escape_sequence;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `'c'` -- a single Unicode scalar value, single-quote delimited. Shares its
/// escape grammar with `StringExpr` (see `syntax::escape`); unlike a string,
/// exactly one character or escape is allowed between the quotes, so there's
/// no `.repeated()` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharExpr(pub char);

impl CharExpr {
    parser!(() => Self {
        just('\'')
            .ignore_then(choice((escape_sequence(), any().and_is(just('\'').not()))))
            .then_ignore(just('\''))
            .trivia_padded()
            .map(Self)
    });
}
