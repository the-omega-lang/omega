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

/// What was found inside `[...]`: either a single index expression, or a
/// `..`-range (a slice). Told apart at the token level -- a range always has
/// a bare `..` in it -- so the postfix fold in `expression/mod.rs` can route
/// straight to `Expression::Index` or `Expression::Slice` without any
/// lookahead of its own.
pub enum IndexPostfix {
    Item(ExpressionNode),
    Range { start: Option<ExpressionNode>, end: Option<ExpressionNode> },
}

impl IndexPostfix {
    parser!((expr_parser => ExpressionNode) => Self {
        let range = expr_parser.clone().or_not()
            .then_ignore(just("..").trivia_padded())
            .then(expr_parser.clone().or_not())
            .map(|(start, end)| Self::Range { start, end });

        let item = expr_parser.map(Self::Item);

        just('[').trivia_padded()
            .ignore_then(choice((range, item)))
            .then_ignore(just(']').trivia_padded())
    });
}
