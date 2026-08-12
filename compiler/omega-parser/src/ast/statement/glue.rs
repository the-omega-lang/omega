use crate::ast::identifier::Path;
use crate::ast::statement::function_definition::FunctionDefinitionStmt;

/// `glue qualified::Gap { function(params) => Return { ... } }` -- the one
/// concrete implementation of a named gap. It has no name or visibility of
/// its own; the target gap supplies the linker namespace.
#[derive(Debug, Clone)]
pub struct GlueStmt {
    pub gap: Path,
    pub functions: Vec<FunctionDefinitionStmt>,
}
