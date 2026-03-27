pub mod expression;
pub mod identifier;
pub mod statement;
pub mod r#type;

use chumsky::error::Rich;

pub type ParseError<'a> = chumsky::extra::Err<Rich<'a, char>>;

#[macro_export]
macro_rules! parser {
    (($($arg:ident => $t:ty),*) => $rt:ty $code:block) => {
        pub fn parser<'a>($($arg: impl Parser<'a, &'a str, $t, ParseError<'a>> + Clone + 'a),*) -> impl Parser<'a, &'a str, $rt, ParseError<'a>> + Clone $code
    };
}
