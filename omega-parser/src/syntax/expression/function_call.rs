use crate::{
    prelude::{Expression, Statement},
    syntax::{ParseError, SyntaxParser, identifier::Ident, r#type::Type},
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub function_ident: Ident,
    pub args: Vec<Expression>,
}

impl SyntaxParser for FunctionCallExpr {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        Ident::parser()
            .then_ignore(just('('))
            .then(
                Expression::parser()
                    .separated_by(just(',').padded())
                    .collect::<Vec<_>>(),
            )
            .map(|(function_ident, args)| Self {
                function_ident,
                args,
            })
    }
}
