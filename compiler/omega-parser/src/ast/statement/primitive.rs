use crate::ast::generics::GenericParam;
use crate::ast::statement::function_definition::FunctionDefinitionStmt;
use crate::ast::r#type::Type;

/// Inherent methods for a compiler-provided type:
/// `primitive<T> []T { exposed method(*self) => ... }`. Target and package
/// restrictions are semantic, not parser concerns.
#[derive(Debug, Clone)]
pub struct PrimitiveStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}
