use crate::{parser, prelude::ExpressionNode};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct ReturnStmt {
    pub return_value: ExpressionNode,
}

impl ReturnStmt {
    parser!((expr_parser => ExpressionNode) => Self {
        text::ascii::keyword("return")
            .padded()
            .ignore_then(expr_parser)
            .map(|expr| Self {
                return_value: expr
            })
    });
}
