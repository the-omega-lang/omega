use crate::ast::expression::ExpressionNode;
use crate::ast::expression::binary_op::BinaryOp;

/// `target op= value` (`+= -= *= /= %= &= |= ^= <<= >>=`) -- parses at the
/// same precedence tier as plain `=` (see `parser::expression::
/// parse_assignment`), just carrying which `BinaryOp` it desugars through.
/// Same "parser doesn't validate `target` is a place" treatment as
/// `AssignmentExpr`.
#[derive(Debug, Clone)]
pub struct CompoundAssignExpr {
    pub target: ExpressionNode,
    pub op: BinaryOp,
    pub value: Box<ExpressionNode>,
}
