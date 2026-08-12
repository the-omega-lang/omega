use crate::ast::identifier::Ident;
use crate::ast::statement::spec::SpecFunctionStmt;

/// `gap Name { function(params) => Return; ... }` -- a named, global
/// platform capability signature. A gap has no visibility, generic, spec, or
/// implementation shape: its functions are declarations only.
#[derive(Debug, Clone)]
pub struct GapStmt {
    pub ident: Ident,
    pub functions: Vec<SpecFunctionStmt>,
}
