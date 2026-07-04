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
    /// A name is declared twice in the same scope (a second parameter with
    /// the same name, or a second local `ident: type;` in the same function
    /// body). Shadowing an *outer* scope is fine and doesn't trigger this.
    Redeclaration(Ident),
    /// An assignment's left-hand side isn't syntactically a place (e.g.
    /// `5 = 3;`) -- rejected here so `CheckedAssignment.target` can be typed
    /// as `CheckedPlace` rather than a general expression.
    AssignmentTargetNotAPlace,
    /// An assignment's value doesn't have the same resolved type as its
    /// target (e.g. assigning a pointer into an `i32` local).
    AssignmentTypeMismatch { target: ResolvedType, value: ResolvedType },
    /// A number literal doesn't fit in its resolved type (only `i32` is
    /// supported today).
    NumberLiteralOutOfRange { literal: String },
    /// `*expr` where `expr`'s resolved type isn't a pointer.
    NotAPointer,
    /// `&expr` where `expr` isn't syntactically a place (e.g. `&5`).
    AddressOfNotAPlace,
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
            Self::Redeclaration(ident) => {
                write!(f, "'{}' is already declared in this scope", ident.as_ref())
            }
            Self::AssignmentTargetNotAPlace => {
                write!(f, "left-hand side of assignment is not an assignable place")
            }
            Self::AssignmentTypeMismatch { target, value } => write!(
                f,
                "cannot assign value of type '{value:?}' to target of type '{target:?}'"
            ),
            Self::NumberLiteralOutOfRange { literal } => {
                write!(f, "number literal '{literal}' does not fit its resolved type")
            }
            Self::NotAPointer => write!(f, "cannot dereference a non-pointer expression"),
            Self::AddressOfNotAPlace => {
                write!(f, "cannot take the address of an expression that is not an assignable place")
            }
        }
    }
}
