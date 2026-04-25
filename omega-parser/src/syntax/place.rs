use chumsky::prelude::*;

use crate::{NodeId, next_node_id, parser, prelude::Ident};

#[derive(Debug, Clone)]
pub enum Place {
    Ident(Ident),
    FieldAccess(Vec<Ident>),
}

#[derive(Debug, Clone)]
pub struct PlaceNode {
    pub id: NodeId,
    pub place: Place,
    pub span: SimpleSpan,
}

impl PlaceNode {
    parser!(() => Self {
        choice((
            Ident::parser().separated_by(just('.')).collect().map(|idents| Place::FieldAccess(idents)),
            Ident::parser().map(|ident| Place::Ident(ident)),
        ))
            .map_with(|place, extra| PlaceNode {
                id: next_node_id(),
                place,
                span: extra.span()
            })
            .padded()
    });
}

impl ToString for Place {
    fn to_string(&self) -> String {
        match self {
            Self::Ident(s) => s.0.to_owned(),
            Self::FieldAccess(idents) => idents
                .iter()
                .map(|i| i.0.clone())
                .collect::<Vec<_>>()
                .join("."),
        }
    }
}
