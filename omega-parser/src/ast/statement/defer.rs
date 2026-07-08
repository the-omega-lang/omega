use crate::ast::statement::Statement;

/// `defer <statement>;` / `defer { ... }` -- schedules `body` to run when
/// the *enclosing function* exits (see `omega_hir::hir::HirDefer` and
/// `omega_codegen`'s epilogue for how). `body` is a bare `Statement`, not a
/// `StatementNode` -- it has no span of its own; lowering reuses the
/// enclosing `defer` statement's span for it, the same way `ForStmt.init`
/// already does for its own wrapped `Statement`.
#[derive(Debug, Clone)]
pub struct DeferStmt {
    pub body: Box<Statement>,
}
