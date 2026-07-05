use crate::parser;
use crate::syntax::ParseError;
use crate::syntax::trivia::TriviaExt;
use chumsky::{error::Rich, prelude::*};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StringExpr(pub String);

/// `\n \t \r \0 \\ \" \' \u{XXXX}` -- the escape sequences a modern language
/// is expected to support inside a string literal. This is its own
/// combinator, tried *before* the plain "any character" fallback in the
/// string body, so a backslash-quote pair (`\"`) is consumed as one atomic
/// unit before the closing-quote check ever sees the quote half of it --
/// otherwise an escaped quote would look exactly like the end of the string.
fn escape_sequence<'a>() -> impl Parser<'a, &'a str, char, ParseError<'a>> + Clone {
    just('\\').ignore_then(choice((
        just('n').to('\n'),
        just('t').to('\t'),
        just('r').to('\r'),
        just('0').to('\0'),
        just('\\').to('\\'),
        just('\'').to('\''),
        just('"').to('"'),
        just('u')
            .ignore_then(
                just('{')
                    .ignore_then(text::digits(16).at_least(1).at_most(6).to_slice())
                    .then_ignore(just('}')),
            )
            .try_map(|hex: &str, span| {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| Rich::custom(span, format!("invalid unicode escape '\\u{{{hex}}}'")))
            }),
    )))
}

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
