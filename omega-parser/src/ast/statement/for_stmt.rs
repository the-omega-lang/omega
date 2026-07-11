use crate::ast::expression::{ExpressionNode, codeblock::CodeblockExpr};
use crate::ast::statement::Statement;

/// `for init; cond; post { ... }` -- classic C-style, three semicolon
/// separated clauses (each independently optional, e.g. `for ;; { ... }` is
/// a valid, deliberately infinite loop) followed by the body. Like `while`,
/// this is a plain statement, never an expression.
///
/// `init` reuses exactly the shapes `Statement` already has for
/// declare-and-assign (`Walrus`, `Declaration`(`WithInit`)) or a plain
/// expression -- but parsed *without* consuming a trailing `;` itself, since
/// the `for` loop's own grammar supplies the `;` separators between clauses.
/// `return`/`extern`/`struct` aren't included: none of them make sense as a
/// loop's init clause.
#[derive(Debug, Clone)]
pub struct ForStmt {
    pub init: Option<Statement>,
    pub condition: Option<ExpressionNode>,
    pub post: Option<ExpressionNode>,
    pub body: CodeblockExpr,
}
