use crate::{
    parser,
    prelude::{Expression, ExpressionNode},
    syntax::{ParseError, identifier::Ident, r#type::Type},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct IndexExpr {
    pub indexed: ExpressionNode,
    pub index: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct LookaheadIndexExpr {
    pub index: ExpressionNode,
}

impl LookaheadIndexExpr {
    parser!((expr_parser => ExpressionNode) => Self {
        just('[').padded()
            .ignore_then(expr_parser)
            .then_ignore(just(']').padded())
            .map(|index| Self { index })
    });
}
