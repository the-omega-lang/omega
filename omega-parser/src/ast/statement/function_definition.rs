use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::codeblock::CodeblockExpr;
use crate::ast::generics::GenericParam;
use crate::ast::identifier::Ident;
use crate::ast::r#type::{FunctionType, Type};
use crate::ast::self_mode::SelfMode;
use crate::ast::statement::declaration::DeclarationStmt;

#[derive(Debug, Clone)]
pub struct FunctionDefinitionStmt {
    /// `@inline(...)`/`@mangling(...)`/`@suppress(...)` written directly
    /// above this function -- applies identically whether this is a
    /// top-level function or a struct/enum/union method, since both are
    /// this same node (see `self_mode`). See `omega_analyzer::annotations`.
    pub annotations: Vec<AnnotationNode>,
    pub ident: Ident,
    /// `<T, U, ...>` immediately after `ident` -- empty for an
    /// ordinary, non-generic function. Unlike a struct's, these are never
    /// referenced with explicit arguments at a call site: they're deduced
    /// from the call's own argument types (see `Analyzer::resolve_generic_call`).
    /// A bound generic (`T: Animal`) additionally requires the deduced
    /// argument type to nominally implement that spec.
    pub generics: Vec<GenericParam>,
    /// `None` for an ordinary, non-member function; `Some` for a
    /// struct/enum/union method, carrying exactly how `self` was written
    /// (`self`/`mut self`/`*self`/`*mut self`) -- determines the
    /// synthesized `self` parameter's type (see
    /// `omega_hir::lower::Lowerer::lower_function_def`).
    pub self_mode: Option<SelfMode>,
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
            self_mode: self.self_mode,
        }
    }
}
