use crate::{
    parser,
    prelude::{Expression, Statement},
    syntax::{ParseError, SyntaxParser, identifier::Ident, r#type::Type},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub function_ident: Ident,
    pub args: Vec<Expression>,
}

impl FunctionCallExpr {
    parser!((expr_parser => Expression) -> Self {
        Ident::parser()
            .then_ignore(just('('))
            .then(
                expr_parser
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>(),
            )
            .map(|(function_ident, args)| Self {
                function_ident,
                args,
            })
            .then_ignore(just(')').padded())
    });
}
