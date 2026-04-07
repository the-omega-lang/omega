use crate::{parser, syntax::ParseError};
use chumsky::prelude::*;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Ident(pub String);

// Helpers for string integration
impl AsRef<str> for Ident {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Parser
impl Ident {
    parser!(() => Self {
        text::ascii::ident().map(|s: &str| Ident(s.to_string()))
    });
}
