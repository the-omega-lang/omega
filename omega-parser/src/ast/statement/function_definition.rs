use crate::ast::expression::codeblock::CodeblockExpr;
use crate::ast::identifier::Ident;
use crate::ast::r#type::{FunctionType, Type};
use crate::ast::statement::declaration::DeclarationStmt;

#[derive(Debug, Clone)]
pub struct FunctionDefinitionStmt {
    pub function_name: Ident,
    /// `<T, U, ...>` immediately after `function_name` -- empty for an
    /// ordinary, non-generic function. Unlike a struct's, these are never
    /// referenced with explicit arguments at a call site: they're deduced
    /// from the call's own argument types (see `Analyzer::resolve_generic_call`).
    pub generics: Vec<Ident>,
    pub is_member_function: bool,
    pub params: Vec<DeclarationStmt>,
    pub return_type: Type,
    pub codeblock: CodeblockExpr,
}

impl FunctionDefinitionStmt {
    pub fn function_type(&self) -> FunctionType {
        let params = self
            .params
            .iter()
            .map(|p| (p.ident.to_owned(), p.r#type.to_owned()))
            .collect::<Vec<_>>();

        FunctionType {
            params,
            return_type: Box::new(self.return_type.clone()),
            is_variadic: false,
            is_member_function: self.is_member_function,
        }
    }
}
