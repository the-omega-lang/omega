use crate::parser;
use chumsky::prelude::*;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Ident(pub String);

// Helpers for string integration
impl AsRef<str> for Ident {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Helpers for string integration
impl ToString for Ident {
    fn to_string(&self) -> String {
        self.0.clone()
    }
}

// Parser
impl Ident {
    parser!(() => Self {
        text::ascii::ident().map(|s: &str| Ident(s.to_string()))
    });
}
