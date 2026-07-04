use crate::{parser, prelude::ExpressionNode};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub callee: Box<ExpressionNode>,
    pub args: Vec<ExpressionNode>,
}

pub struct FunctionCallPostfix {
    pub args: Vec<ExpressionNode>,
}

impl FunctionCallPostfix {
    parser!((expr_parser => ExpressionNode) => Self {
        just('(').trivia_padded()
            .ignore_then(
                expr_parser
                    .separated_by(just(',').trivia_padded())
                    .collect::<Vec<_>>()
            )
            .map(|args| Self {
                args,
            })
            .then_ignore(just(')').trivia_padded())
    });
}
