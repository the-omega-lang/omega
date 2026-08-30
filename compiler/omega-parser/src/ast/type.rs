use crate::ast::expression::NumberExpr;
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

impl Param {
    /// This binding's appearance inside a function type, where its name is
    /// only a descriptor.
    pub fn as_function_type_param(&self) -> FunctionTypeParam {
        FunctionTypeParam::described(self.ident.clone(), self.span, self.r#type.clone())
    }
}

impl PartialEq for Param {
    fn eq(&self, other: &Self) -> bool {
        self.ident == other.ident && self.r#type == other.r#type
    }
}

impl Eq for Param {}

/// A parameter written inside a function type. Unlike a declaration
/// [`Param`], it binds nothing: `name` is optional descriptive metadata kept
/// for diagnostics and tooling, so structural equality compares only the
/// parameter type.
#[derive(Debug, Clone)]
pub struct FunctionTypeParam {
    pub name: Option<Ident>,
    pub span: Span,
    pub r#type: Type,
}

impl FunctionTypeParam {
    pub fn described(name: Ident, span: Span, r#type: Type) -> Self {
        Self {
            name: Some(name),
            span,
            r#type,
        }
    }
}

impl PartialEq for FunctionTypeParam {
    fn eq(&self, other: &Self) -> bool {
        self.r#type == other.r#type
    }
}

impl Eq for FunctionTypeParam {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<FunctionTypeParam>,
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
    SizedArray(Box<Type>, ArrayLength),
    Generic(Path, Vec<GenericArg>),
    SpecStatic(Vec<Type>),
    /// `enum A | B | ...`: a structural sum whose variants are the member
    /// types. Written order is preserved here; canonical ordering and
    /// deduplication are semantic and happen during type resolution.
    AnonymousEnum(Vec<Type>),
}

/// A scalar value written directly inside generic-argument or array-length
/// syntax. The set is deliberately small: richer compile-time values are
/// written once as a `comp` binding and passed by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompLiteral {
    Int { negative: bool, number: NumberExpr },
    Bool(bool),
    Char(char),
}

/// One written generic argument. A bare path stays `Type(Type::Named(..))`:
/// whether it denotes a type or a `comp` value is decided by the declared
/// parameter's kind during resolution, never guessed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericArg {
    Type(Type),
    Value(CompLiteral),
}

impl GenericArg {
    /// The type syntax this argument is written as, when it is not a scalar
    /// value literal. Syntax-only passes (traversal, alias substitution,
    /// display) work through this.
    pub fn as_type(&self) -> Option<&Type> {
        match self {
            Self::Type(r#type) => Some(r#type),
            Self::Value(_) => None,
        }
    }
}

impl From<Type> for GenericArg {
    fn from(r#type: Type) -> Self {
        Self::Type(r#type)
    }
}

/// A fixed array's written length: a scalar literal, or a path naming a
/// `comp` binding or `comp` generic parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayLength {
    Literal(CompLiteral),
    Path(Path),
}
