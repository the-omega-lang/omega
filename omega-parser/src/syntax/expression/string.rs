use crate::{parser, syntax::ParseError};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct StringExpr(pub String);

impl StringExpr {
    parser!(() -> Self {
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
        //     .map(|s| StringExpr(s));
    });
}
