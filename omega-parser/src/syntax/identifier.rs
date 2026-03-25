use crate::syntax::{ParseError, SyntaxParser};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct Ident(pub String);

// Helpers for string integration
impl AsRef<str> for Ident {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<String> for Ident {
    fn as_ref(&self) -> &String {
        &self.0
    }
}

// Parser
impl SyntaxParser for Ident {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        text::ascii::ident().map(|s: &str| Ident(s.to_string()))
    }
}
