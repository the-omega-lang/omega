use crate::parser;
use crate::syntax::escape::escape_sequence;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StringExpr(pub String);

impl StringExpr {
    parser!(() => Self {
        // NOTE: this used to also support Python-triple-quote-style
        // delimiters of any odd repeated-quote count (`"""..."""`,
        // `"""""..."""""`, ...) via a quote-counting `configure`/
        // `ignore_with_ctx` scheme. That feature was never used by any real
        // code and its own comments already flagged it as fragile ("sometimes
        // it matches things that dont start with \""); layering escape
        // sequences on top of it correctly (an escaped quote must never
        // count towards closing a 1-quote-delimited string) would only have
        // made it more so. Dropped in favor of the plain, standard
        // single-double-quote string every language example in this repo
        // actually uses.
        let content = choice((escape_sequence(), any().and_is(just('"').not())))
            .repeated()
            .collect::<String>();

        just('"')
            .ignore_then(content)
            .then_ignore(just('"'))
            .trivia_padded()
            .map(Self)
    });
}
