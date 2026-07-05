use crate::syntax::ParseError;
use chumsky::{error::Rich, prelude::*};

/// `\n \t \r \0 \\ \" \' \u{XXXX}` -- the escape sequences a modern language
/// is expected to support inside a string or char literal. This is its own
/// combinator, tried *before* the plain "any character" fallback in a
/// literal's body, so a backslash-quote pair (`\"`/`\'`) is consumed as one
/// atomic unit before the closing-quote check ever sees the quote half of it
/// -- otherwise an escaped quote would look exactly like the end of the
/// literal. Shared between `expression::string` and `expression::char_literal`
/// rather than duplicated, since both need exactly the same escape grammar.
pub fn escape_sequence<'a>() -> impl Parser<'a, &'a str, char, ParseError<'a>> + Clone {
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
