use crate::ast::expression::ExpressionNode;

/// `&base` -- a plain expression-forming prefix operator. Unlike `Deref`,
/// this never denotes a place itself (it produces a pointer *value*); the
/// parser still doesn't validate that `base` is addressable, that's HIR
/// lowering/analysis's job, same as an assignment's target.
#[derive(Debug, Clone)]
pub struct AddressOfExpr {
    pub base: ExpressionNode,
}
