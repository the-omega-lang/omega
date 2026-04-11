use crate::{parser, syntax::ParseError};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct StringExpr(pub String);

impl StringExpr {
    parser!(() => Self {
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

        // NOTE: The below implementation may be closer to what is required to be done
        // let quotes = just('"').repeated();
        // let delim_start = quotes.count();
        // let delim_end = quotes.configure(|cfg, ctx| cfg.exactly(*ctx));
        // let content = any().and_is(delim_end.not()).repeated().collect::<String>();

        // delim_start
        //     .ignore_with_ctx(content.then_ignore(delim_end))
        //     .map(|s| StringExpr(s))


        // NOTE: The below implementation somewhat worked, but now sometimes it matches things that dont start with "
        // let empty_string = just('"').repeated().exactly(2).padded().map(|_| StringExpr("".to_owned()));

        // let delim_start = just('"').repeated().exactly(1).ignore_then(just("\"\"").repeated().count().map(|count| count * 2 + 1)).or(just('"').not().map(|_| 0));
        // let delim_end = just('"').repeated().configure(|cfg, ctx| cfg.exactly(*ctx));
        // let content = any().and_is(just('"').not()).then(any().and_is(delim_end.not()).repeated().collect::<String>()).map(|(first, remainder)| format!("{}{}", first, remainder));
        // let longquote_string = delim_start
        //     .ignore_with_ctx(content.then_ignore(delim_end))
        //     .padded()
        //     .map(|s| StringExpr(s));

        // choice(
        //     (longquote_string, empty_string)
        // )
    });
}
