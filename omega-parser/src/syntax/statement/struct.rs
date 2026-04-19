use std::collections::HashMap;

use crate::{
    parser,
    prelude::DeclarationStmt,
    syntax::{ParseError, identifier::Ident, r#type::Type},
};
use chumsky::{prelude::*, text::ascii::keyword};

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub ident: Ident,
    pub fields: Vec<DeclarationStmt>,
}

impl StructStmt {
    parser!((decl_parser => DeclarationStmt) => Self {
        keyword("struct").padded()
            .ignore_then(Ident::parser().padded())
            .then_ignore(just('{').padded())
            .then(decl_parser.padded().separated_by(just(';').padded()).at_least(1).collect())
            .then_ignore(just(';').padded())
            .then_ignore(just('}').padded())
            .map(|(ident, fields)| Self { ident, fields })
    });
}
