use crate::prelude::ExpressionNode;

/// Assignment is right-associative and has the lowest precedence of any
/// expression form -- built directly as the outermost layer of
/// `ExpressionNode::parser` (see `super`), not as a generic postfix operator
/// like `FieldAccess`/`Index`/`Call`.
#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub target: ExpressionNode,
    pub value: Box<ExpressionNode>,
}
