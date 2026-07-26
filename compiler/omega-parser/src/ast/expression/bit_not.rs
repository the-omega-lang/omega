use crate::ast::expression::ExpressionNode;

/// `~base` -- a plain expression-forming prefix operator, same rationale as
/// [`super::negate::NegateExpr`]: unary bitwise-not, integer-only (rejected
/// for `Bool`/`Char`/`Float` during analysis, same as `-`'s own operand
/// restriction).
#[derive(Debug, Clone)]
pub struct BitNotExpr {
    pub base: ExpressionNode,
}
