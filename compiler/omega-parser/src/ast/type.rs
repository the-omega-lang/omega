use crate::ast::identifier::{Ident, Origin, Path};
use crate::diagnostics::Span;
use crate::ast::self_mode::SelfMode;

#[derive(Debug, Clone)]
pub struct Param {
    pub ident: Ident,
    pub name_span: Span,
    pub span: Span,
    pub origin: Origin,
    pub r#type: Type,
}

impl PartialEq for Param {
    fn eq(&self, other: &Self) -> bool {
        self.ident == other.ident && self.r#type == other.r#type
    }
}

impl Eq for Param {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<Param>,
    pub return_type: Box<Type>,
    pub is_variadic: bool,
    pub self_mode: Option<SelfMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Named(Path),
    Pointer(Box<Type>, bool),
    Function(FunctionType),
    InferredArray(Box<Type>),
    UnknownSizeArray(Box<Type>),
    SizedArray(Box<Type>, String),
    Generic(Path, Vec<Type>),
    SpecObject(Box<Type>, bool),
    SpecStatic(Box<Type>),
}
