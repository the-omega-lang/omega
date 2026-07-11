use crate::ast::expression::ExpressionNode;

/// `-base` -- a plain expression-forming prefix operator, same rationale as
/// [`super::deref::DerefExpr`]. Added alongside binary subtraction: without
/// it there would be no way to write a negative value or negate a variable
/// (`NumberExpr`'s grammar has no sign of its own).
#[derive(Debug, Clone)]
pub struct NegateExpr {
    pub base: ExpressionNode,
}
