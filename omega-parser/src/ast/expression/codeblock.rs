use crate::ast::expression::ExpressionNode;
use crate::ast::statement::StatementNode;

/// `{ stmt; stmt; ... tail }` -- `tail` is the optional final expression with
/// no trailing `;`, whose value is the block's own value.
#[derive(Debug, Clone)]
pub struct CodeblockExpr {
    pub statements: Vec<StatementNode>,
    pub tail: Option<Box<ExpressionNode>>,
}
