use crate::prelude::ExpressionNode;

/// A plain data tag, no parser-specific structure -- reused unchanged
/// through HIR, analysis, and codegen the same way `Ident`/`Type` already
/// are, rather than re-wrapped at each layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// `left op right` -- a plain expression-forming operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser only knows this is
/// syntax, not whether/how it type-checks.
#[derive(Debug, Clone)]
pub struct BinaryOpExpr {
    pub left: ExpressionNode,
    pub op: BinaryOp,
    pub right: ExpressionNode,
}
