use crate::{parser, prelude::Ident, syntax::ParseError};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct NumberExpr {
    pub integer_part: String,
    pub fractional_part: Option<String>,
    pub explicit_type: Option<Ident>,
}

impl NumberExpr {
    parser!(() => Self {
        let digit_parser = text::digits(10).at_least(1).to_slice().map(ToString::to_string);
        let explicit_type_parser =
            choice((just('i'), just('u'), just('f')))
                .then(digit_parser.clone())
                .map(|(prefix, digits)| Ident(format!("{prefix}{digits}")));

        digit_parser
            .then(just('.').ignore_then(digit_parser.clone()).or_not())
            .then(explicit_type_parser.or_not())
            .map(|((integer_part, fractional_part), explicit_type)| Self {
                integer_part, fractional_part, explicit_type
            })
    });
}
