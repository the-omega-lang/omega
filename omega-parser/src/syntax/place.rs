use chumsky::prelude::*;

use crate::{
    parser,
    prelude::{ExpressionNode, Ident},
};

#[derive(Debug, Clone)]
pub struct PlaceExpr {
    pub base: ExpressionNode,
    pub modifier: PlaceModifierPostfix,
}

#[derive(Debug, Clone)]
pub enum PlaceModifierPostfix {
    FieldAccess(Ident),
    Index(ExpressionNode),
}

impl PlaceModifierPostfix {
    parser!((expr_parser => ExpressionNode) => Self {
        choice((
            just('.').padded().ignore_then(Ident::parser().padded()).map(|ident| PlaceModifierPostfix::FieldAccess(ident)),
            just('[').padded()
                .ignore_then(expr_parser)
                .then_ignore(just(']').padded())
                .map(|expr| PlaceModifierPostfix::Index(expr))
        ))
    });
}
