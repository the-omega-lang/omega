use crate::{parser, prelude::ExpressionNode};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `base[index]` -- a plain expression-forming operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser doesn't know or care
/// whether this denotes an addressable location, only HIR lowering does.
#[derive(Debug, Clone)]
pub struct IndexExpr {
    pub base: ExpressionNode,
    pub index: ExpressionNode,
}

pub struct IndexPostfix {
    pub index: ExpressionNode,
}

impl IndexPostfix {
    parser!((expr_parser => ExpressionNode) => Self {
        just('[').trivia_padded()
            .ignore_then(expr_parser)
            .then_ignore(just(']').trivia_padded())
            .map(|index| Self { index })
    });
}
