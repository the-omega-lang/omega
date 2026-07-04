use crate::{parser, prelude::ExpressionNode};
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
        just('(').padded()
            .ignore_then(
                expr_parser
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>()
            )
            .map(|args| Self {
                args,
            })
            .then_ignore(just(')').padded())
    });
}
