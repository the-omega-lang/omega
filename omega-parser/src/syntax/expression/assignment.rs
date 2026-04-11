use crate::{
    parser,
    prelude::ExpressionNode,
    syntax::{ParseError, identifier::Ident, r#type::Type},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub ident: Ident,
    pub value: Box<ExpressionNode>,
}

impl AssignmentExpr {
    parser!((expr_parser => ExpressionNode) => Self {
        Ident::parser()
            .padded()
            .then_ignore(just('=').padded())
            .then(expr_parser)
            .map(|(ident, value)| Self { ident, value: Box::new(value) })
    });
}
