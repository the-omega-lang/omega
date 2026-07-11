use crate::ast::expression::ExpressionNode;

#[derive(Debug, Clone)]
pub struct ReturnStmt {
    pub return_value: ExpressionNode,
}
