use crate::{
    parser,
    syntax::{identifier::Ident, r#type::Type},
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
}

impl DeclarationStmt {
    parser!(() => Self {
        Ident::parser()
            .trivia_padded()
            .then_ignore(just(':').trivia_padded())
            .then(Type::parser())
            .map(|(ident, typ)| Self { ident, r#type: typ })
    });
}
