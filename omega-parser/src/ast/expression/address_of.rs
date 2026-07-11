use crate::ast::expression::ExpressionNode;

/// `&base` (`mutable: false`) or `&mut base` (`mutable: true`) -- a plain
/// expression-forming prefix operator. Unlike `Deref`, this never denotes a
/// place itself (it produces a pointer *value*); the parser still doesn't
/// validate that `base` is addressable (or, for `&mut`, mutable), that's
/// HIR lowering/analysis's job, same as an assignment's target.
#[derive(Debug, Clone)]
pub struct AddressOfExpr {
    pub base: ExpressionNode,
    pub mutable: bool,
}
