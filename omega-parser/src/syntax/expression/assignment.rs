use crate::{
    parser,
    prelude::ExpressionNode,
    syntax::{
        ParseError,
        identifier::Ident,
        place::{Place, PlaceNode},
        r#type::Type,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub place: PlaceNode,
    pub value: Box<ExpressionNode>,
}

impl AssignmentExpr {
    parser!((expr_parser => ExpressionNode) => Self {
        PlaceNode::parser(expr_parser.clone())
            .padded()
            .then_ignore(just('=').padded())
            .then(expr_parser)
            .map(|(place, value)| Self { place, value: Box::new(value) })
    });
}
