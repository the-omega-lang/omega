use crate::ast::generics::GenericParam;
use crate::ast::statement::function_definition::FunctionDefinitionStmt;
use crate::ast::r#type::Type;

/// A nominal conformance declaration: `conform<T> Target<T> to Spec<T> { ... }`.
/// The block itself is unnamed; member visibility comes from the matched spec
/// requirement rather than surface modifiers in this list.
#[derive(Debug, Clone)]
pub struct ConformStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub spec: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}
