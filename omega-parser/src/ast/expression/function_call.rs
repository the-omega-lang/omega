use crate::ast::expression::ExpressionNode;

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub callee: Box<ExpressionNode>,
    pub args: Vec<ExpressionNode>,
}
