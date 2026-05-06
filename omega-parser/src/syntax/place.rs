use chumsky::prelude::*;

use crate::{
    NodeId, next_node_id, parser,
    prelude::{ExpressionNode, Ident},
};

#[derive(Debug, Clone)]
pub struct Place(pub Ident, pub Vec<PlaceModifier>);

#[derive(Debug, Clone)]
pub struct PlaceNode {
    pub id: NodeId,
    pub place: Place,
    pub span: SimpleSpan,
}

#[derive(Debug, Clone)]
pub enum PlaceModifier {
    FieldAccess(Ident),
    Index(ExpressionNode),
}

impl PlaceNode {
    parser!((expr_parser => ExpressionNode) => Self {
        let place_modifier_parser =
            choice((
                just('.').padded().ignore_then(Ident::parser().padded()).map(|ident| PlaceModifier::FieldAccess(ident)),
                just('[').padded()
                    .ignore_then(expr_parser)
                    .then_ignore(just(']').padded())
                    .map(|expr| PlaceModifier::Index(expr))
            ));

        Ident::parser()
            .then(place_modifier_parser.repeated().collect())
            .map_with(|(ident, modifiers), extra| {
                PlaceNode { id: next_node_id(), place: Place(ident, modifiers), span: extra.span() }
            })
    });
}
