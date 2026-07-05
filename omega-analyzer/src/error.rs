use crate::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Ident, SimpleSpan};
use std::fmt;

#[derive(Debug, Clone)]
pub enum TypeResolutionError {
    UnrecognizedNamedType(Ident),
    /// `[T; N]`'s `N` doesn't fit `u32` -- kept as raw text by the parser
    /// (same as `NumberExpr`'s integer literals) and only parsed/range-checked
    /// here, during type resolution.
    InvalidArraySize(String),
}

impl fmt::Display for TypeResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnrecognizedNamedType(ident) => {
                write!(f, "unrecognized named type: {}", ident.as_ref())
            }
            Self::InvalidArraySize(size) => {
                write!(f, "array size '{size}' does not fit a u32")
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
    /// A `+ - * / %` operand isn't `i32` (the only numeric type today).
    InvalidBinaryOperand { op: BinaryOp, r#type: ResolvedType },
    /// A unary `-` operand isn't `i32`.
    InvalidNegateOperand { r#type: ResolvedType },
    /// `base[start..end]` where `base`'s resolved type is neither
    /// `SizedArray` nor `Slice`.
    NotSliceable,
    /// A slice's `start`/`end` bound isn't `i32`.
    InvalidSliceBound { r#type: ResolvedType },
    /// `[]` -- there's no element to infer the array's item type from.
    EmptyArrayLiteral,
    /// An array literal's elements don't all share the same resolved type
    /// (the first element's type is what every other element is checked
    /// against).
    ArrayElementTypeMismatch { expected: ResolvedType, found: ResolvedType },
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
            Self::InvalidBinaryOperand { op, r#type } => write!(
                f,
                "cannot use operand of type '{type:?}' with operator '{op:?}' (only i32 is supported)"
            ),
            Self::InvalidNegateOperand { r#type } => write!(
                f,
                "cannot negate operand of type '{type:?}' (only i32 is supported)"
            ),
            Self::NotSliceable => {
                write!(f, "cannot slice an expression that is not a sized array or a slice")
            }
            Self::InvalidSliceBound { r#type } => write!(
                f,
                "slice bound must be of type 'i32', found '{type:?}'"
            ),
            Self::EmptyArrayLiteral => {
                write!(f, "cannot infer the element type of an empty array literal")
            }
            Self::ArrayElementTypeMismatch { expected, found } => write!(
                f,
                "array literal element of type '{found:?}' does not match preceding elements of type '{expected:?}'"
            ),
        }
    }
}
