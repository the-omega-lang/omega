pub mod expression;
pub mod identifier;
pub mod statement;
pub mod r#type;

use chumsky::error::Rich;

pub type ParseError<'a> = chumsky::extra::Err<Rich<'a, char>>;

#[macro_export]
macro_rules! parser {
    (($($arg:ident => $t:ty),*) => $rt:ty $code:block) => {
        pub fn parser<'a>($($arg: impl chumsky::prelude::Parser<'a, &'a str, $t, crate::syntax::ParseError<'a>> + Clone + 'a),*) -> impl chumsky::prelude::Parser<'a, &'a str, $rt, crate::syntax::ParseError<'a>> + Clone $code
    };
}
