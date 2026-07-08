use crate::resolved_type::ResolvedType;
use crate::resolver::ResolveError;
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Ident, Span};
use std::fmt;

#[derive(Debug, Clone)]
pub enum TypeResolutionError {
    UnrecognizedNamedType(Ident),
    /// `[T; N]`'s `N` doesn't fit `u32` -- kept as raw text by the parser
    /// (same as `NumberExpr`'s integer literals) and only parsed/range-checked
    /// here, during type resolution.
    InvalidArraySize(String),
    /// A qualified type path (`mymodule::Foo`) failed to resolve across
    /// modules -- unknown module/item, not visible, or a cycle. See
    /// `crate::resolver::ModuleResolver`.
    ModuleResolution(ResolveError),
    /// A qualified path resolved to a value (a function/extern/global), not
    /// a type, in a position that requires a type.
    NotAType(Vec<Ident>),
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
            Self::ModuleResolution(e) => write!(f, "{e}"),
            Self::NotAType(path) => write!(
                f,
                "'{}' is a value, not a type",
                path.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join("::")
            ),
        }
    }
}

impl std::error::Error for TypeResolutionError {}

#[derive(Debug, Clone)]
pub struct AnalysisError {
    pub node_id: HirId,
    pub span: Span,
    pub kind: AnalysisErrorKind,
}

impl AnalysisError {
    pub fn new(node_id: HirId, span: Span, kind: AnalysisErrorKind) -> Self {
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
    /// A `+ - * / %` operand's types don't match each other (e.g. `i32 +
    /// i64`) -- unlike `InvalidBinaryOperand`, both operands *are* numeric,
    /// they just aren't the same numeric type; this language has no implicit
    /// numeric conversions, so a mismatch here is always an error rather than
    /// a promotion.
    BinaryOperandTypeMismatch { left: ResolvedType, right: ResolvedType },
    /// `%` (`BinaryOp::Rem`) applied to a float operand -- there's no native
    /// floating-point remainder instruction to lower this to (matching C,
    /// which requires calling `fmod`/`fmodf` instead of using `%`).
    FloatRemainder,
    /// An `if`/`while`/`for` condition doesn't resolve to `Bool`.
    NonBoolCondition { r#type: ResolvedType },
    /// An `if`/`else if`/`else` branch's resolved type doesn't match the
    /// others (see `Analyzer::block_type`/the `HirExpr::If` arm for exactly
    /// how "the others" is determined, including how a branch that diverges
    /// via `return` is exempt).
    IfBranchTypeMismatch { expected: ResolvedType, found: ResolvedType },
    /// A function's body doesn't produce its declared return type -- neither
    /// a tail expression of the right type, nor an unconditional trailing
    /// `return`, nor (for `Void`) falling off the end with no tail at all.
    /// Also used for an individual `return <expr>;` whose type doesn't match
    /// the enclosing function's declared return type.
    ReturnTypeMismatch { expected: ResolvedType, found: ResolvedType },
    /// `++expr`/`--expr` where `expr` isn't syntactically a place (e.g.
    /// `++5`).
    IncrementTargetNotAPlace,
    /// `++expr`/`--expr` where `expr`'s resolved type isn't numeric (e.g.
    /// `bool`, `char`, or a pointer).
    InvalidIncrementOperand { r#type: ResolvedType },
    /// `for init;; post { ... }` -- the condition clause was omitted. Unlike
    /// `init`/`post`, this isn't just a style choice: this language has no
    /// constant-condition reasoning to prove an always-true loop's exit
    /// point is ever actually reached, which codegen can't soundly build a
    /// jump target for (every cranelift block must end in a terminator) --
    /// see `CheckedFor`'s doc comment.
    ForLoopMissingCondition,
    /// `break;` outside any enclosing `while`/`for`.
    BreakOutsideLoop,
    /// `continue;` outside any enclosing `while`/`for`.
    ContinueOutsideLoop,
    /// A qualified place/value path (`mymodule::foo`) failed to resolve
    /// across modules -- unknown module/item, not visible, or a cycle. See
    /// `crate::resolver::ModuleResolver`.
    ModuleResolution(crate::resolver::ResolveError),
    /// A qualified path resolved to a type (a struct), not a value, in a
    /// position that requires a value (e.g. calling it, or using it as a
    /// place).
    NotAValue(Vec<Ident>),
    /// A generic function call's argument-driven type inference
    /// (`Analyzer::resolve_generic_call`) couldn't deduce a concrete type
    /// for this declared generic parameter -- it never appeared (in a
    /// structurally recognizable position) in any of the call's arguments.
    UnresolvedGenericParam(Ident),
    /// A struct or function declared with generic parameters (`<T, ...>`)
    /// inside a function body (a locally-nested `HirStmt::Struct`, or one of
    /// its methods) -- generics are only supported on top-level items, which
    /// have a stable cross-module identity to key an instantiation by;
    /// a locally-nested definition has none.
    NestedGenericsNotSupported,
    /// `defer` lexically inside a `while`/`for` loop body -- out of scope for
    /// now. A `defer`'s "was this reached" tracking is a single runtime
    /// boolean flag (see `omega_codegen`'s `defer_flags`), which can't
    /// represent "reached N times"; correct per-iteration defer needs a real
    /// dynamic, variable-length deferred-call list, which is real future
    /// work, not this version's scope.
    DeferInsideLoopNotSupported,
    /// `return` inside a `defer`'s own body. Deferred code only ever runs
    /// from the enclosing function's shared epilogue (see `omega_codegen`),
    /// so a `return` here would have to jump into that very epilogue from
    /// inside code the epilogue itself is running -- not supported.
    ReturnInsideDefer,
    /// A `defer` statement nested inside another `defer`'s own body --
    /// not supported; a defer's body always runs at most once per function
    /// call already, and there is no useful "defer whose scope is another
    /// defer's body" to speak of, only the enclosing function's exit.
    NestedDeferNotSupported,
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
                "cannot use operand of type '{type:?}' with operator '{op:?}' (a numeric type is required)"
            ),
            Self::InvalidNegateOperand { r#type } => write!(
                f,
                "cannot negate operand of type '{type:?}' (only signed integers and floats are supported)"
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
            Self::BinaryOperandTypeMismatch { left, right } => write!(
                f,
                "binary operator operands have different types: '{left:?}' and '{right:?}'"
            ),
            Self::FloatRemainder => {
                write!(f, "'%' is not supported on floating-point operands")
            }
            Self::NonBoolCondition { r#type } => write!(
                f,
                "condition must be of type 'bool', found '{type:?}'"
            ),
            Self::IfBranchTypeMismatch { expected, found } => write!(
                f,
                "'if' branch of type '{found:?}' does not match preceding branches of type '{expected:?}'"
            ),
            Self::ReturnTypeMismatch { expected, found } => write!(
                f,
                "expected return type '{expected:?}', found '{found:?}'"
            ),
            Self::IncrementTargetNotAPlace => {
                write!(f, "'++'/'--' operand is not an assignable place")
            }
            Self::InvalidIncrementOperand { r#type } => write!(
                f,
                "cannot increment/decrement operand of type '{type:?}' (a numeric type is required)"
            ),
            Self::ForLoopMissingCondition => {
                write!(f, "'for' loop is missing its condition clause")
            }
            Self::BreakOutsideLoop => write!(f, "'break' outside of a loop"),
            Self::ContinueOutsideLoop => write!(f, "'continue' outside of a loop"),
            Self::ModuleResolution(e) => write!(f, "{e}"),
            Self::NotAValue(path) => write!(
                f,
                "'{}' is a type, not a value",
                path.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join("::")
            ),
            Self::UnresolvedGenericParam(ident) => write!(
                f,
                "cannot infer type parameter '{}' from this call's arguments",
                ident.as_ref()
            ),
            Self::NestedGenericsNotSupported => {
                write!(f, "generics are not supported on locally-nested structs/functions")
            }
            Self::DeferInsideLoopNotSupported => {
                write!(f, "'defer' is not supported inside a loop body")
            }
            Self::ReturnInsideDefer => write!(f, "'return' is not supported inside a 'defer' body"),
            Self::NestedDeferNotSupported => write!(f, "'defer' is not supported inside another 'defer' body"),
        }
    }
}

/// A non-fatal analysis finding: unlike `AnalysisError`, this never rejects
/// the program (see `Analyzer::analyze`'s return type) -- it's a place to
/// hang findings a future diagnostics pass would surface to the user (e.g.
/// as compiler warnings), without building that reporting machinery now.
#[derive(Debug, Clone)]
pub struct AnalysisWarning {
    pub node_id: HirId,
    pub span: Span,
    pub kind: AnalysisWarningKind,
}

impl AnalysisWarning {
    pub fn new(node_id: HirId, span: Span, kind: AnalysisWarningKind) -> Self {
        Self { node_id, span, kind }
    }
}

impl fmt::Display for AnalysisWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

#[derive(Debug, Clone)]
pub enum AnalysisWarningKind {
    /// A statement that can never run: it follows something that
    /// unconditionally diverges (`return`/`break`/`continue`, or an
    /// `if`/`else` where every branch diverges) in the same block. Dropped
    /// before codegen ever sees it (see `Analyzer::analyze_stmts`/
    /// `analyze_block`) rather than risking codegen emitting instructions
    /// into an already-terminated cranelift block.
    UnreachableCode,
}

impl fmt::Display for AnalysisWarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnreachableCode => write!(f, "unreachable code"),
        }
    }
}
