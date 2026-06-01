use omega_parser::prelude::{ExpressionNode, Ident, PlaceModifierPostfix};

#[derive(Debug, Clone)]
pub enum PlaceRoot {
    Ident(Ident),
    Deref(ExpressionNode),
}

#[derive(Debug, Clone)]
pub struct Place {
    pub root: PlaceRoot,
    pub modifiers: Vec<PlaceModifierPostfix>,
}
