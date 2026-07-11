use crate::ast::expression::{ExpressionNode, codeblock::CodeblockExpr};

/// `while cond { ... }` -- a plain statement, not an expression: unlike
/// `if`, a loop's body may run zero or many times, so there's no single
/// "the value it produced" to speak of (this language has no `break
/// <value>` either).
#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub condition: ExpressionNode,
    pub body: CodeblockExpr,
}
