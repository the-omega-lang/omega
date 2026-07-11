use crate::ast::expression::ExpressionNode;

/// `base[index]` -- a plain expression-forming operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser doesn't know or care
/// whether this denotes an addressable location, only HIR lowering does.
#[derive(Debug, Clone)]
pub struct IndexExpr {
    pub base: ExpressionNode,
    pub index: ExpressionNode,
}
