use crate::{
    parser,
    prelude::{ExpressionNode, Ident},
};
use chumsky::prelude::*;

/// `base.field` -- a plain expression-forming operator. The parser has no
/// notion of "places"/lvalues; it just knows this syntax exists. Whether a
/// given `FieldAccessExpr` chain denotes an addressable location is decided
/// later, during HIR lowering.
#[derive(Debug, Clone)]
pub struct FieldAccessExpr {
    pub base: ExpressionNode,
    pub field: Ident,
}

pub struct FieldAccessPostfix {
    pub field: Ident,
}

impl FieldAccessPostfix {
    parser!(() => Self {
        just('.')
            .padded()
            .ignore_then(Ident::parser().padded())
            .map(|field| Self { field })
    });
}
