use crate::ast::expression::ExpressionNode;

/// Assignment is right-associative and has the lowest precedence of any
/// expression form -- built directly as the outermost layer of expression
/// parsing (see `crate::parser::expression`), not as a generic postfix
/// operator like `FieldAccess`/`Index`/`Call`.
#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub target: ExpressionNode,
    pub value: Box<ExpressionNode>,
}
