use crate::ast::expression::ExpressionNode;

/// `*base` -- a plain expression-forming prefix operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser doesn't know or care
/// that this denotes an addressable location, only HIR lowering does (this
/// one folds into a place the same way `FieldAccess`/`Index` do).
#[derive(Debug, Clone)]
pub struct DerefExpr {
    pub base: ExpressionNode,
}
