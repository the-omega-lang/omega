use crate::ast::expression::ExpressionNode;

/// `[e1, e2, ...]` -- a fixed-size array value, one element expression per
/// slot. Unlike `Type::SizedArray`, the size isn't written down here: it's
/// just however many elements are listed, the same way `NumberExpr` doesn't
/// carry its own resolved type -- semantic analysis is what turns "N
/// elements" into a `ResolvedType::SizedArray(item, N)`.
#[derive(Debug, Clone)]
pub struct ArrayLiteralExpr {
    pub elements: Vec<ExpressionNode>,
}
