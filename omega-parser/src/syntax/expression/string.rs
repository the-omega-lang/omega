use crate::syntax::{ParseError, SyntaxParser};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct StringExpr(String);

impl SyntaxParser for StringExpr {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        just('"')
            .ignore_then(none_of('"').repeated().to_slice().map(ToString::to_string))
            .then_ignore(just('"'))
            .map(|s| Self(s))

        // TODO: Implement odd-lengthed delimiter strings, similar to as follows
        // let quote_start_parser = just('"').repeated().at_least(1).count();

        // let str_remainder_parser = |quote_count| {
        //     let str_terminator = just('"').repeated().exactly(quote_count);
        //     none_of(str_terminator.clone())
        //         .repeated()
        //         .collect::<String>()
        //         .then_ignore(str_terminator)
        // };

        // quote_start_parser.then(str_remainder_parser);
    }
}
