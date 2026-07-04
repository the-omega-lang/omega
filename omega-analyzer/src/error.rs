use crate::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::{Ident, SimpleSpan};
use std::fmt;

#[derive(Debug, Clone)]
pub enum TypeResolutionError {
    UnrecognizedNamedType(Ident),
}

impl fmt::Display for TypeResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrecognizedNamedType(ident) => {
                write!(f, "unrecognized named type: {}", ident.as_ref())
            }
        }
    }
}

impl std::error::Error for TypeResolutionError {}

#[derive(Debug, Clone)]
pub struct AnalysisError {
    pub node_id: HirId,
    pub span: SimpleSpan,
    pub kind: AnalysisErrorKind,
}

impl AnalysisError {
    pub fn new(node_id: HirId, span: SimpleSpan, kind: AnalysisErrorKind) -> Self {
        Self {
            node_id,
            span,
            kind,
        }
    }
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for AnalysisError {}

#[derive(Debug, Clone)]
pub enum AnalysisErrorKind {
    UnresolvedType(TypeResolutionError),
    UndefinedVariable(Ident),
    NotAStruct,
    NoSuchField(Ident),
    NotAnArray,
    TooManyArguments { expected: usize },
    ArgumentTypeMismatch { expected: ResolvedType, found: ResolvedType },
    UnresolvedCallee,
    InvalidNumberType(Ident),
    UnresolvedInnerExpression,
    FunctionTypeLookupFailed,
}

impl fmt::Display for AnalysisErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedType(e) => write!(f, "{e}"),
            Self::UndefinedVariable(ident) => write!(f, "undefined variable '{}'", ident.as_ref()),
            Self::NotAStruct => write!(f, "not a struct"),
            Self::NoSuchField(ident) => write!(f, "no such field '{}' in struct", ident.as_ref()),
            Self::NotAnArray => write!(f, "not an array"),
            Self::TooManyArguments { expected } => {
                write!(f, "too many arguments for function, expected {expected}")
            }
            Self::ArgumentTypeMismatch { expected, found } => write!(
                f,
                "expected type '{expected:?}' for argument, found '{found:?}'"
            ),
            Self::UnresolvedCallee => write!(f, "callee does not resolve to a callable function"),
            Self::InvalidNumberType(ident) => write!(
                f,
                "invalid explicit type for number expression: '{}'",
                ident.as_ref()
            ),
            Self::UnresolvedInnerExpression => write!(f, "inner expression could not be resolved"),
            Self::FunctionTypeLookupFailed => {
                write!(f, "failed to look up the type of a just-analyzed function")
            }
        }
    }
}
