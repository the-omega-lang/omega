use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;
use crate::diagnostics::Span;

#[derive(Debug, Clone)]
pub struct AnnotationNode {
    pub name: Ident,
    pub args: Vec<AnnotationArg>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum AnnotationArg {
    Ident(Ident),
    KeyValue(Ident, AnnotationValue),
}

#[derive(Debug, Clone)]
pub enum AnnotationValue {
    IntLiteral(String),
    Sizeof(Type),
    StrLiteral(String),
}
