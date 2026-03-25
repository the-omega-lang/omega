use crate::syntax::{ParseError, SyntaxParser, identifier::Ident, r#type::Type};
use chumsky::prelude::*;

// This is NOT a statement.
// This is a declaration template used
// in the statements that parse declarations
// (e.g extern and actual declaration)
pub struct BaseDeclaration {
    pub ident: Ident,
    pub r#type: Type,
}

impl SyntaxParser for BaseDeclaration {
    fn parser<'a>() -> impl chumsky::Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        Ident::parser()
            .padded()
            .then_ignore(just(':').padded())
            .then(Type::parser())
            .map(|(ident, typ)| Self { ident, r#type: typ })
    }
}
