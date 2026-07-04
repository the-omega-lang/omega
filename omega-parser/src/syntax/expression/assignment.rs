use crate::{parser, prelude::ExpressionNode};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub target: ExpressionNode,
    pub value: Box<ExpressionNode>,
}

#[derive(Debug, Clone)]
pub struct AssignmentPostfix {
    pub value: ExpressionNode,
}

impl AssignmentPostfix {
    parser!((expr_parser => ExpressionNode) => Self {
        just('=').padded()
            .ignore_then(expr_parser)
            .map(|value| Self { value })
    });
}
