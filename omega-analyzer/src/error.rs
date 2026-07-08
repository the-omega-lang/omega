use crate::resolved_type::{NumericKind, ResolvedType};
use crate::resolver::ResolveError;
use omega_diagnostics::Diagnostic;
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Ident, Span};
use std::fmt;

fn join(path: &[Ident]) -> String {
    path.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join("::")
}

#[derive(Debug, Clone)]
pub enum TypeResolutionError {
    /// A bare type name that doesn't exist in scope. `similar` is a
    /// close-enough visible type name, when one exists -- the "did you
    /// mean" candidate (computed at error time, while the scope still
    /// exists to search).
    UnrecognizedNamedType { name: Ident, similar: Option<Ident> },
    /// A qualified reference (`mymodule::Foo`) whose head was never bound
    /// by an `import` -- nothing is visible across modules without one.
    ModuleNotImported { name: Ident, similar: Option<Ident> },
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
            Self::UnrecognizedNamedType { name, .. } => {
                write!(f, "cannot find type '{}' in this scope", name.as_ref())
            }
            Self::ModuleNotImported { name, .. } => {
                write!(f, "module '{}' is not imported", name.as_ref())
            }
            Self::InvalidArraySize(size) => {
                write!(f, "array size '{size}' does not fit a u32")
            }
            Self::ModuleResolution(e) => write!(f, "{e}"),
            Self::NotAType(path) => write!(f, "'{}' is a value, not a type", join(path)),
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

    /// The renderable form of this error: a headline stating the problem, a
    /// caret label localizing it, and -- where a language rule or a likely
    /// fix genuinely helps -- a `note:`/`help:` footer. Advice is only
    /// attached where it's always true; a wrong hint is worse than none.
    pub fn to_diagnostic(&self) -> Diagnostic {
        self.kind.to_diagnostic(self.span)
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
    /// An unqualified name that resolves to nothing visible. `similar` is a
    /// close-enough visible name, when one exists.
    UndefinedVariable { name: Ident, similar: Option<Ident> },
    /// A qualified place/value path (`mymodule::foo`) whose head was never
    /// bound by an `import` -- the value counterpart of
    /// `TypeResolutionError::ModuleNotImported`.
    ModuleNotImported { name: Ident, similar: Option<Ident> },
    /// A field access on something that isn't a struct (after auto-deref).
    NotAStruct { found: ResolvedType },
    /// A field access naming a field `base` doesn't have.
    NoSuchField { field: Ident, base: ResolvedType },
    /// An index projection on something that isn't an array/slice.
    NotAnArray { found: ResolvedType },
    /// A call supplying the wrong number of arguments (too many *or* too
    /// few -- despite this once being named `TooManyArguments`).
    WrongArgumentCount { expected: usize, found: usize },
    ArgumentTypeMismatch { expected: ResolvedType, found: ResolvedType },
    UnresolvedCallee,
    InvalidNumberType(Ident),
    UnresolvedInnerExpression,
    /// A name is declared twice in the same scope (a second parameter with
    /// the same name, or a second local `ident: type;` in the same function
    /// body). Shadowing an *outer* scope is fine and doesn't trigger this.
    /// `previous` is the first declaration's span, when the declaring site
    /// tracks one -- rendered as a "first declared here" secondary label.
    Redeclaration { name: Ident, previous: Option<Span> },
    /// An assignment's left-hand side isn't syntactically a place (e.g.
    /// `5 = 3;`) -- rejected here so `CheckedAssignment.target` can be typed
    /// as `CheckedPlace` rather than a general expression.
    AssignmentTargetNotAPlace,
    /// An assignment's value doesn't have the same resolved type as its
    /// target (e.g. assigning a pointer into an `i32` local).
    AssignmentTypeMismatch { target: ResolvedType, value: ResolvedType },
    /// A number literal doesn't fit in its resolved type.
    NumberLiteralOutOfRange { literal: String, r#type: ResolvedType },
    /// `*expr` where `expr`'s resolved type isn't a pointer.
    NotAPointer { found: ResolvedType },
    /// `&expr` where `expr` isn't syntactically a place (e.g. `&5`).
    AddressOfNotAPlace,
    /// A `+ - * / %` operand isn't numeric.
    InvalidBinaryOperand { op: BinaryOp, r#type: ResolvedType },
    /// A unary `-` operand isn't a signed integer or float.
    InvalidNegateOperand { r#type: ResolvedType },
    /// `base[start..end]` where `base`'s resolved type is neither
    /// `SizedArray` nor `Slice`.
    NotSliceable { found: ResolvedType },
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
    /// a promotion. The per-operand spans let the diagnostic point at each
    /// side with its own type.
    BinaryOperandTypeMismatch { left: ResolvedType, left_span: Span, right: ResolvedType, right_span: Span },
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

impl AnalysisErrorKind {
    /// See `AnalysisError::to_diagnostic`. `span` is the error's own anchor
    /// span, which every primary label lands on unless the kind carries
    /// something more precise (e.g. `BinaryOperandTypeMismatch`'s per-operand
    /// spans).
    pub fn to_diagnostic(&self, span: Span) -> Diagnostic {
        let d = Diagnostic::error(self.to_string());
        match self {
            Self::UnresolvedType(e) => type_resolution_diagnostic(e, span),
            Self::UndefinedVariable { similar, .. } => {
                let d = d.with_label(span, "not found in this scope");
                match similar {
                    Some(name) => d.with_help(format!("a name with a similar spelling exists: `{}`", name.as_ref())),
                    None => d,
                }
            }
            Self::ModuleNotImported { name, similar } => {
                let d = d
                    .with_label(span, "this module is not in scope")
                    .with_help(format!("add `import {};` at the top of the file", name.as_ref()));
                match similar {
                    Some(alias) => {
                        d.with_help(format!("an imported module with a similar name exists: `{}`", alias.as_ref()))
                    }
                    None => d,
                }
            }
            Self::NotAStruct { found } => d
                .with_label(span, format!("this has type `{found}`, which has no fields"))
                .with_note("only struct values (and pointers to them) support field access"),
            Self::NoSuchField { base, .. } => d.with_label(span, format!("`{base}` has no field by that name")),
            Self::NotAnArray { found } => d
                .with_label(span, format!("this has type `{found}`, which cannot be indexed"))
                .with_note("only arrays (`[T; N]`, `[T]`) and slices (`*[T]`) support indexing"),
            Self::WrongArgumentCount { expected, found } => {
                d.with_label(span, format!("expected {expected} {}, found {found}", plural(*expected, "argument")))
            }
            Self::ArgumentTypeMismatch { expected, found } => d
                .with_label(span, format!("expected `{expected}`, found `{found}`"))
                .with_note("Omega has no implicit conversions; each argument must match its parameter's type exactly"),
            Self::UnresolvedCallee => d.with_label(span, "this is not callable"),
            Self::InvalidNumberType(_) => d.with_label(span, "not a numeric type").with_note(
                "valid numeric types are i8 i16 i32 i64 isize, u8 u16 u32 u64 usize, and f32 f64",
            ),
            Self::UnresolvedInnerExpression => d.with_label(span, "could not resolve this expression"),
            Self::Redeclaration { name, previous } => {
                let d = d.with_label(span, format!("`{}` declared again here", name.as_ref())).with_note(
                    "a name can only be declared once per scope; shadowing an outer scope is allowed",
                );
                match previous {
                    Some(previous) => {
                        d.with_secondary_label(*previous, format!("`{}` first declared here", name.as_ref()))
                    }
                    None => d,
                }
            }
            Self::AssignmentTargetNotAPlace => d
                .with_label(span, "cannot assign to this expression")
                .with_note("only variables, fields, indexes, and dereferences can be assigned to"),
            Self::AssignmentTypeMismatch { target, value } => d
                .with_label(span, format!("expected `{target}`, found `{value}`"))
                .with_note("Omega has no implicit conversions; the value must have exactly the target's type"),
            Self::NumberLiteralOutOfRange { r#type, .. } => {
                let d = d.with_label(span, format!("does not fit in `{}`", r#type));
                match type_range(r#type) {
                    Some(range) => d.with_note(format!("`{}` can hold values from {range}", r#type)),
                    None => d,
                }
            }
            Self::NotAPointer { found } => {
                d.with_label(span, format!("this has type `{found}`, which cannot be dereferenced"))
            }
            Self::AddressOfNotAPlace => d
                .with_label(span, "cannot take the address of this expression")
                .with_note("only values with a memory location (variables, fields, indexes, dereferences) have an address"),
            Self::InvalidBinaryOperand { op, r#type } => d
                .with_label(span, format!("`{}` requires numeric operands, but this is `{}`", op.symbol(), r#type)),
            Self::InvalidNegateOperand { r#type } => d
                .with_label(span, format!("this has type `{}`", r#type))
                .with_note("unary `-` requires a signed integer or a float"),
            Self::NotSliceable { found } => d
                .with_label(span, format!("this has type `{found}`, which cannot be sliced"))
                .with_note("only sized arrays (`[T; N]`) and slices (`*[T]`) support `[start..end]`"),
            Self::InvalidSliceBound { r#type } => {
                d.with_label(span, format!("slice bounds must be `i32`, found `{}`", r#type))
            }
            Self::EmptyArrayLiteral => d
                .with_label(span, "cannot infer what `[]` holds")
                .with_note("an array literal's type comes from its first element"),
            Self::ArrayElementTypeMismatch { expected, found } => d
                .with_label(span, format!("expected `{expected}`, found `{found}`"))
                .with_note("every element of an array literal must have the first element's type"),
            Self::BinaryOperandTypeMismatch { left, left_span, right, right_span } => d
                .with_secondary_label(*left_span, format!("this is `{left}`"))
                .with_label(*right_span, format!("this is `{right}`"))
                .with_note("Omega has no implicit numeric conversions; both operands must have exactly the same type"),
            Self::FloatRemainder => d
                .with_label(span, "`%` requires integer operands")
                .with_note("there is no native float remainder instruction (C's `%` is integer-only too)"),
            Self::NonBoolCondition { r#type } => d
                .with_label(span, format!("expected `bool`, found `{}`", r#type))
                .with_note("conditions must be `bool`; there is no implicit truthiness"),
            Self::IfBranchTypeMismatch { expected, found } => d
                .with_label(span, format!("this branch produces `{found}`, but earlier branches produce `{expected}`"))
                .with_note("every branch of an `if` used as an expression must produce the same type"),
            Self::ReturnTypeMismatch { expected, found } => {
                d.with_label(span, format!("expected `{expected}` because of the declared return type, found `{found}`"))
            }
            Self::IncrementTargetNotAPlace => d
                .with_label(span, "cannot increment/decrement this expression")
                .with_note("`++`/`--` need somewhere to store the result: a variable, field, index, or dereference"),
            Self::InvalidIncrementOperand { r#type } => {
                d.with_label(span, format!("`++`/`--` require a numeric operand, but this is `{}`", r#type))
            }
            Self::ForLoopMissingCondition => d
                .with_label(span, "this `for` has no condition clause")
                .with_help("write `for init; condition; post { ... }`, or use `while true { ... }` for an intentionally infinite loop"),
            Self::BreakOutsideLoop => d.with_label(span, "cannot `break` outside of a `while`/`for` loop"),
            Self::ContinueOutsideLoop => d.with_label(span, "cannot `continue` outside of a `while`/`for` loop"),
            Self::ModuleResolution(e) => resolve_error_diagnostic(e, Some(span)),
            Self::NotAValue(_) => d
                .with_label(span, "expected a value, found a type")
                .with_note("a struct's name refers to the type itself; only its instances hold values"),
            Self::UnresolvedGenericParam(name) => d
                .with_label(span, format!("cannot deduce `{}` from this call's arguments", name.as_ref()))
                .with_note("a generic function's type parameters are deduced from its argument types only"),
            Self::NestedGenericsNotSupported => d
                .with_label(span, "generic parameters are not allowed here")
                .with_note("generics are only supported on top-level structs and functions"),
            Self::DeferInsideLoopNotSupported => d
                .with_label(span, "`defer` cannot appear inside a loop body")
                .with_help("move the `defer` outside the loop, or run the cleanup code directly"),
            Self::ReturnInsideDefer => d
                .with_label(span, "cannot `return` from inside a `defer` body")
                .with_note("deferred code runs while the function is already returning"),
            Self::NestedDeferNotSupported => d
                .with_label(span, "`defer` cannot appear inside another `defer` body")
                .with_note("a defer's body already runs exactly once, at function exit"),
        }
    }
}

/// See `AnalysisErrorKind::to_diagnostic` -- same shape, for the inner
/// type-resolution errors.
fn type_resolution_diagnostic(error: &TypeResolutionError, span: Span) -> Diagnostic {
    let d = Diagnostic::error(error.to_string());
    match error {
        TypeResolutionError::UnrecognizedNamedType { similar, .. } => {
            let d = d.with_label(span, "not found in this scope");
            match similar {
                Some(name) => d.with_help(format!("a type with a similar name exists: `{}`", name.as_ref())),
                None => d,
            }
        }
        TypeResolutionError::ModuleNotImported { name, similar } => {
            let d = d
                .with_label(span, "this module is not in scope")
                .with_help(format!("add `import {};` at the top of the file", name.as_ref()));
            match similar {
                Some(alias) => {
                    d.with_help(format!("an imported module with a similar name exists: `{}`", alias.as_ref()))
                }
                None => d,
            }
        }
        TypeResolutionError::InvalidArraySize(_) => d
            .with_label(span, "array size out of range")
            .with_note("an array's length must fit in a `u32`"),
        TypeResolutionError::ModuleResolution(e) => resolve_error_diagnostic(e, Some(span)),
        TypeResolutionError::NotAType(_) => {
            d.with_label(span, "expected a type, found a value")
        }
    }
}

/// The renderable form of a module-resolution failure. `span` is the
/// referencing site (an `import` statement, a qualified path, ...) when the
/// caller has one; a `None` renders headline/footers only.
pub fn resolve_error_diagnostic(error: &ResolveError, span: Option<Span>) -> Diagnostic {
    let d = Diagnostic::error(error.to_string());
    let with_label = |d: Diagnostic, message: String| match span {
        Some(span) => d.with_label(span, message),
        None => d,
    };
    match error {
        ResolveError::UnknownModule(path) => {
            let name = path.last().map(|i| i.as_ref()).unwrap_or_default();
            with_label(d, "module not found".to_string()).with_note(format!(
                "modules are looked up as `{name}.omg` files or `{name}/` directories under the compiler's search roots"
            ))
        }
        ResolveError::UnknownItem { module, .. } => with_label(d, format!("not found in `{}`", join(module))),
        ResolveError::NotVisible { .. } => with_label(d, "not visible from this module".to_string()),
        ResolveError::Cycle(_) => with_label(d, "this import completes the cycle".to_string())
            .with_note("modules whose imports mutually depend on each other cannot be resolved"),
        ResolveError::AmbiguousModule(path) => {
            let name = path.last().map(|i| i.as_ref()).unwrap_or_default();
            with_label(d, "ambiguous module reference".to_string())
                .with_help(format!("keep either the `{name}.omg` file or the `{name}/` directory, not both"))
        }
        ResolveError::LoadFailed { .. } => with_label(d, "imported from here".to_string()),
        ResolveError::MacroExpansionFailed { .. } => with_label(d, "imported from here".to_string()),
        ResolveError::RecursiveTypeWithoutIndirection { item, .. } => {
            with_label(d, format!("`{}` includes itself by value, giving it infinite size", item.as_ref())).with_help(
                format!("insert indirection (e.g. a pointer: `*{}`) somewhere in the cycle", item.as_ref()),
            )
        }
        ResolveError::ItemFailed { item, .. } => {
            with_label(d, "cannot be used because of its own error".to_string())
                .with_note(format!("`{}`'s own error is reported where it is defined", item.as_ref()))
        }
        ResolveError::GenericArgCountMismatch { expected, .. } => {
            with_label(d, format!("expected {expected} type {}", plural(*expected, "argument")))
        }
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_string() } else { format!("{word}s") }
}

/// The inclusive value range of a numeric type, for
/// `NumberLiteralOutOfRange`'s note -- `None` for floats (their "range" is
/// about precision, not simple bounds, so a bounds note would mislead).
fn type_range(r#type: &ResolvedType) -> Option<String> {
    match r#type.numeric_kind()? {
        NumericKind::Signed(bits) => {
            let max = (1u128 << (bits - 1)) - 1;
            Some(format!("-{} to {max}", max + 1))
        }
        NumericKind::Unsigned(bits) => {
            let max = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
            Some(format!("0 to {max}"))
        }
        NumericKind::Float(_) => None,
    }
}

impl fmt::Display for AnalysisErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedType(e) => write!(f, "{e}"),
            Self::UndefinedVariable { name, .. } => write!(f, "cannot find '{}' in this scope", name.as_ref()),
            Self::ModuleNotImported { name, .. } => write!(f, "module '{}' is not imported", name.as_ref()),
            Self::NotAStruct { found } => write!(f, "field access on '{found}', which is not a struct"),
            Self::NoSuchField { field, base } => {
                write!(f, "no field '{}' on '{base}'", field.as_ref())
            }
            Self::NotAnArray { found } => write!(f, "cannot index a value of type '{found}'"),
            Self::WrongArgumentCount { expected, found } => {
                write!(f, "this function takes {expected} {} but {found} {} supplied",
                    plural(*expected, "argument"),
                    if *found == 1 { "was" } else { "were" })
            }
            Self::ArgumentTypeMismatch { expected, found } => write!(
                f,
                "mismatched types: expected '{expected}' for this argument, found '{found}'"
            ),
            Self::UnresolvedCallee => write!(f, "this expression is not a callable function"),
            Self::InvalidNumberType(ident) => write!(
                f,
                "invalid numeric type '{}' for a number literal",
                ident.as_ref()
            ),
            Self::UnresolvedInnerExpression => write!(f, "inner expression could not be resolved"),
            Self::Redeclaration { name, .. } => {
                write!(f, "'{}' is declared multiple times in this scope", name.as_ref())
            }
            Self::AssignmentTargetNotAPlace => {
                write!(f, "invalid assignment target")
            }
            Self::AssignmentTypeMismatch { target, value } => write!(
                f,
                "mismatched types: cannot assign '{value}' to a target of type '{target}'"
            ),
            Self::NumberLiteralOutOfRange { literal, r#type } => {
                write!(f, "number '{literal}' does not fit in '{}'", r#type)
            }
            Self::NotAPointer { found } => write!(f, "cannot dereference a value of type '{found}'"),
            Self::AddressOfNotAPlace => {
                write!(f, "cannot take the address of this expression")
            }
            Self::InvalidBinaryOperand { op, r#type } => write!(
                f,
                "cannot apply '{}' to a value of type '{}'",
                op.symbol(),
                r#type
            ),
            Self::InvalidNegateOperand { r#type } => write!(
                f,
                "cannot negate a value of type '{}'", r#type
            ),
            Self::NotSliceable { found } => {
                write!(f, "cannot slice a value of type '{found}'")
            }
            Self::InvalidSliceBound { r#type } => write!(
                f,
                "mismatched types: slice bound must be 'i32', found '{}'", r#type
            ),
            Self::EmptyArrayLiteral => {
                write!(f, "cannot infer the element type of an empty array literal")
            }
            Self::ArrayElementTypeMismatch { .. } => {
                write!(f, "mismatched types in array literal")
            }
            Self::BinaryOperandTypeMismatch { left, right, .. } => write!(
                f,
                "mismatched types: '{left}' and '{right}'"
            ),
            Self::FloatRemainder => {
                write!(f, "'%' is not supported on floating-point operands")
            }
            Self::NonBoolCondition { r#type } => write!(
                f,
                "mismatched types: condition must be 'bool', found '{}'", r#type
            ),
            Self::IfBranchTypeMismatch { .. } => write!(
                f,
                "'if' and 'else' branches have incompatible types"
            ),
            Self::ReturnTypeMismatch { expected, found } => write!(
                f,
                "mismatched types: expected return type '{expected}', found '{found}'"
            ),
            Self::IncrementTargetNotAPlace => {
                write!(f, "invalid '++'/'--' operand")
            }
            Self::InvalidIncrementOperand { r#type } => write!(
                f,
                "cannot increment/decrement a value of type '{}'", r#type
            ),
            Self::ForLoopMissingCondition => {
                write!(f, "this 'for' loop is missing its condition clause")
            }
            Self::BreakOutsideLoop => write!(f, "'break' outside of a loop"),
            Self::ContinueOutsideLoop => write!(f, "'continue' outside of a loop"),
            Self::ModuleResolution(e) => write!(f, "{e}"),
            Self::NotAValue(path) => write!(f, "'{}' is a type, not a value", join(path)),
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
/// the program (see `Analyzer::analyze`'s return type) -- surfaced to the
/// user as a rendered `warning:` diagnostic, but compilation proceeds.
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

    pub fn to_diagnostic(&self) -> Diagnostic {
        let d = Diagnostic::warning(self.kind.to_string());
        match self.kind {
            AnalysisWarningKind::UnreachableCode => d
                .with_label(self.span, "this can never run")
                .with_note("it follows something that always diverges (`return`, `break`, or `continue`)"),
        }
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
