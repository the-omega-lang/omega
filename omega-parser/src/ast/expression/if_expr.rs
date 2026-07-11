use crate::ast::expression::{ExpressionNode, codeblock::CodeblockExpr};

/// `if cond { ... } else if cond { ... } else { ... }` -- a genuine
/// expression (unlike `while`/`for`), whose value is whichever branch's
/// block ran (see `CodeblockExpr`'s tail expression). `branches` holds every
/// `if`/`else if` condition-block pair in source order (the first entry is
/// always the leading `if`); `else_branch` is the trailing `else`, if any.
/// Analysis is what enforces that every branch (and the `else`, if present)
/// resolves to the same type -- the parser only knows the shape.
#[derive(Debug, Clone)]
pub struct IfExpr {
    pub branches: Vec<(ExpressionNode, CodeblockExpr)>,
    pub else_branch: Option<CodeblockExpr>,
}
