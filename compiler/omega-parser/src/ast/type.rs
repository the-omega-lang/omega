use crate::ast::identifier::{Ident, Origin, Path};
use crate::ast::self_mode::SelfMode;
use crate::diagnostics::Span;

/// A calling-convention name written at the source level, e.g. the `c` in
/// `foreign(c)`. Kept raw (unresolved) through parsing/HIR; semantic
/// resolution against the target happens in the analyzer. Equality ignores
/// `span` so it does not perturb `FunctionType`/`Type` structural equality.
#[derive(Debug, Clone)]
pub struct RawConvention {
    pub name: Ident,
    pub span: Span,
}

impl PartialEq for RawConvention {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for RawConvention {}

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
    /// `None` denotes the ordinary Omega convention; explicit `foreign(cc)`
    /// type syntax is the only source of `Some`.
    pub convention: Option<RawConvention>,
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
    SpecStatic(Vec<Type>),
    /// `enum A | B | ...`: a structural sum whose variants are the member
    /// types. Written order is preserved here; canonical ordering and
    /// deduplication are semantic and happen during type resolution.
    AnonymousEnum(Vec<Type>),
}
