use crate::{
    parser,
    prelude::Expression,
    syntax::{ParseError, expression::ExpressionNode, identifier::Ident},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub function_name: Ident,
    pub args: Vec<ExpressionNode>,
}

impl FunctionCallExpr {
    parser!((expr_parser => ExpressionNode) => Self {
        Ident::parser()
            .then_ignore(just('('))
            .then(
                expr_parser
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>()
            )
            .map(|(function_name, args)| Self {
                function_name,
                args,
            })
            .then_ignore(just(')').padded())
    });
}
