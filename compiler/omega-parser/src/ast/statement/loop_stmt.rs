use crate::ast::expression::codeblock::CodeblockExpr;

/// `loop { ... }` -- an unconditional loop, exiting only via `break` (or
/// never, if none is reached). Unlike `WhileStmt`, there is no condition
/// at all: this is the one shape the analyzer can prove always repeats
/// unless a `break` targeting it is found anywhere in its own body (see
/// `Analyzer::stmt_diverges`), which is what lets a function ending in
/// `loop { }` satisfy a `never` return type. Still a plain statement, not
/// an expression, for the same reason `WhileStmt` is (see its own doc
/// comment) -- this language has no `break <value>`.
#[derive(Debug, Clone)]
pub struct LoopStmt {
    pub body: CodeblockExpr,
}
