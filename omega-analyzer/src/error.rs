use crate::resolved_type::{NumericKind, ResolvedFunctionType, ResolvedType};
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
    /// `Enum::Name` in *type* position (e.g. `x: *Entity::Name`) where
    /// `Name` isn't one of `Enum`'s variants -- the type-position mirror of
    /// `AnalysisErrorKind::NoSuchEnumMember`.
    NoSuchVariantForType { r#enum: Ident, name: Ident, similar: Option<Ident> },
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
            Self::NoSuchVariantForType { r#enum, name, .. } => {
                write!(f, "no variant '{}' on enum '{}'", name.as_ref(), r#enum.as_ref())
            }
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
    /// A qualified place/value path (`head::rest`) whose head names nothing
    /// visible: not an imported module alias, not a type, and not one of
    /// this module's own items. What the user *meant* can't be known (a
    /// module they forgot to import, or a typo'd struct name), so this
    /// carries a "did you mean" candidate from each world and only ever
    /// suggests what actually exists.
    UndefinedPathHead { name: Ident, similar_module: Option<Ident>, similar_type: Option<Ident> },
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
    /// A compound assignment's (`+= -= *= /= %= &= |= ^= <<= >>=`)
    /// left-hand side isn't syntactically a place -- same reasoning as
    /// `AssignmentTargetNotAPlace`.
    CompoundAssignTargetNotAPlace,
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
    /// A unary `~` operand isn't a signed or unsigned integer.
    InvalidBitNotOperand { r#type: ResolvedType },
    /// A `& | ^ << >>` operand is a float -- there's no native instruction
    /// for any of these on floating-point operands.
    FloatBitwiseOperand,
    /// `base[start..end]` where `base`'s resolved type is neither
    /// `SizedArray` nor `Slice`.
    NotSliceable { found: ResolvedType },
    /// `base[start..end]` written without a leading `&`/`&mut` -- a slice
    /// expression alone doesn't say whether it should be immutable or
    /// mutable.
    SliceRequiresAddressOf,
    /// `&mut base[start..end]` where `base` is itself an already-immutable
    /// `Slice` value -- distinct from `NotMutableBinding`/`NotMutablePointer`
    /// because the *binding* holding the slice may well be `mut`; it's the
    /// slice value's own flag that's immutable.
    ImmutableSliceSource,
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
    /// `Name { field = value; ... }` where `Name` resolves to a type that
    /// isn't a struct or union (a primitive, an array, ...).
    StructLiteralNotAStruct { found: ResolvedType },
    /// A struct literal setting the same field twice. `previous` is the
    /// first initializer's span -- rendered as a "first set here" label.
    DuplicateFieldInitializer { field: Ident, previous: Span },
    /// A struct literal field's value doesn't have the field's declared
    /// type.
    FieldTypeMismatch { field: Ident, expected: ResolvedType, found: ResolvedType },
    /// A struct literal that doesn't cover every declared field -- partial
    /// initialization is not allowed (there is no implicit zeroing).
    MissingFieldInitializers { r#struct: Ident, missing: Vec<Ident> },
    /// `Struct::function` naming a function `Struct` doesn't have. `similar`
    /// is a close-enough function name on that struct, when one exists.
    NoSuchStructFunction { r#struct: Ident, function: Ident, similar: Option<Ident> },
    /// `Struct::function(...)` where `function` takes `self` -- a member
    /// function needs an instance to be called on.
    MemberFunctionWithoutInstance { r#struct: Ident, function: Ident },
    /// `value.function(...)` where `function` does *not* take `self` -- a
    /// static function is called through the struct's name, not an instance.
    StaticFunctionOnInstance { r#struct: Ident, function: Ident },
    /// `Type::name` where `Type` is a real type but not a struct (e.g.
    /// `i32::something`) -- only structs can have functions.
    StaticAccessOnNonStruct { found: ResolvedType },
    /// `Struct::function::more` -- a path trying to reach *through* a
    /// struct's function; functions have no items of their own.
    StructPathTooDeep { r#struct: Ident, function: Ident },
    /// `head::item` where `head` names a *value* (a function or global) --
    /// values have no items of their own; only modules and struct types do.
    NotAModule { name: Ident },
    /// An enum header entry named `tag` that isn't the *first* entry -- the
    /// tag is required to lead the header (it's how the runtime layout
    /// starts, and how the compiler tells variants apart).
    EnumTagNotFirst,
    /// An explicit tag (`tag: T` leading the header) whose `T` isn't an
    /// integer type -- tags are currently always numeric.
    EnumTagNotInteger { found: ResolvedType },
    /// A header field whose type has no compile-time-constant literal form
    /// (a struct, an array, ...) -- header values are per-variant constants,
    /// so every header field must be expressible as one.
    EnumHeaderFieldUnsupportedType { field: Ident, found: ResolvedType },
    /// A variant supplying the wrong number of header values. `expected`
    /// counts the explicit tag when the enum declares one (`has_tag`), so
    /// the message can spell out what the list must contain.
    EnumVariantArgCount { variant: Ident, expected: usize, found: usize, has_tag: bool },
    /// A variant's tag/header value that isn't a literal constant -- the
    /// header is per-variant *constant* data, baked in at the definition.
    EnumValueNotConstant,
    /// A variant's tag/header value whose literal kind can't be a value of
    /// the field's declared type (e.g. a string where `u32` is expected).
    /// `found` is a short description of what was written.
    EnumValueTypeMismatch { expected: ResolvedType, found: String },
    /// Two variants sharing one tag value -- tags are how variants are told
    /// apart at runtime, so they must be unique per variant.
    DuplicateEnumTag { variant: Ident, value: String, previous_variant: Ident, previous: Span },
    /// A variant body field with the same name as a shared header field --
    /// both are reached as `value.name`, so they must not collide. Also
    /// covers a body/header field named `tag` (`header_field` is `tag`
    /// then), which the tag itself already claims on every enum.
    EnumFieldShadowsHeader { field: Ident, variant: Option<Ident> },
    /// `Enum { ... }` -- an enum can't be built by naming just the enum; a
    /// specific variant must be chosen. `example` is a real variant of this
    /// enum, for the help text.
    EnumLiteralWithoutVariant { r#enum: Ident, example: Ident },
    /// `Enum::Name`/`Enum::Name { ... }` where `Name` is neither a variant
    /// nor a function of the enum. Carries a "did you mean" candidate from
    /// each namespace; only ever suggests what actually exists.
    NoSuchEnumMember { r#enum: Ident, name: Ident, similar_variant: Option<Ident>, similar_function: Option<Ident> },
    /// `Enum::Variant` (bare, no `{ ... }`) where the variant declares body
    /// fields -- they'd be left uninitialized, and there is no implicit
    /// zeroing anywhere in this language.
    EnumVariantMissingBody { r#enum: Ident, variant: Ident, fields: Vec<Ident> },
    /// `Enum::Variant { ... }` where the variant declares *no* body fields.
    EnumVariantHasNoBody { r#enum: Ident, variant: Ident },
    /// `Struct::Name { ... }` -- a literal path reaching into a struct as
    /// if it had variants.
    StructLiteralPathTooDeep { r#struct: Ident, name: Ident },
    /// A field access naming a *body* field of a different variant than the
    /// one this value statically is.
    EnumFieldWrongVariant { field: Ident, owner: Ident, actual: Ident },
    /// A field access naming a body field on an enum value whose variant
    /// isn't statically known -- without knowing the variant, the field may
    /// not exist in the value at all. `owner` is the variant declaring it.
    EnumFieldVariantUnknown { field: Ident, r#enum: Ident, owner: Ident },
    /// A field access naming something that is neither the tag, a header
    /// field, nor any variant's body field.
    NoSuchEnumField { field: Ident, r#enum: Ident, similar: Option<Ident> },
    /// A path with explicit generic arguments (`Optional<u32>::...`)
    /// continuing more than one segment past the instantiated type --
    /// nothing nests deeper than a type's own members.
    GenericPathTooDeep { r#type: Ident },
    /// An assignment to an enum value's tag or one of its header fields --
    /// both are per-variant constants; only a variant's own body fields are
    /// mutable.
    EnumFieldImmutable { field: Ident },
    /// A variant-body literal (`Enum::Variant { ... }`) trying to set a
    /// *header* field -- header values are fixed per variant by the enum's
    /// own definition, never supplied at a construction site.
    EnumHeaderFieldInLiteral { field: Ident },

    // -- match expressions --
    /// A `match` pattern's value/range bound isn't a literal constant --
    /// unlike an ordinary expression, a pattern is checked against the
    /// scrutinee's whole domain at compile time, so its bounds have to be
    /// known then too.
    PatternValueNotConstant,
    /// `Enum::Name` as a match pattern where `Name` isn't one of `Enum`'s
    /// variants. The pattern-position mirror of `NoSuchEnumMember` (patterns
    /// only ever name a variant, never a function, so there's just one
    /// candidate namespace here).
    NoSuchVariantInPattern { r#enum: Ident, name: Ident, similar: Option<Ident> },
    /// A value/range pattern (`100`, `0..<10`) matched against an enum
    /// scrutinee -- an enum can only be matched by naming one of its
    /// variants.
    PatternNotEnumVariant { r#enum: Ident },
    /// An `Enum::Variant` pattern matched against a non-enum scrutinee.
    PatternIsEnumVariant { r#enum: Ident, variant: Ident, scrutinee: ResolvedType },
    /// A value/range pattern's own type doesn't match the scrutinee's exact
    /// type (e.g. a `u32` scrutinee matched against an `i32` literal --
    /// this language has no implicit numeric conversions anywhere else
    /// either).
    PatternTypeMismatch { expected: ResolvedType, found: ResolvedType },
    /// `match`'s scrutinee isn't a supported type -- scoped to enums,
    /// integers, and `bool` for now (see `ResolvedType::integer_domain`).
    UnsupportedMatchScrutinee { r#type: ResolvedType },
    /// Two arms' patterns cover the same value -- an enum variant named by
    /// more than one arm, or two numeric/`bool` patterns whose intervals
    /// intersect. `previous` points at the earlier, already-covering arm.
    OverlappingMatchArm { previous: Span },
    /// An enum `match` covers only some variants and has no `else` --
    /// `missing` lists every variant left uncovered.
    NonExhaustiveMatchEnum { r#enum: Ident, missing: Vec<Ident> },
    /// A numeric/`bool` `match` doesn't cover its scrutinee's whole domain
    /// and has no `else` -- `gaps` describes each uncovered sub-range.
    NonExhaustiveMatchValue { r#type: ResolvedType, gaps: Vec<String> },
    /// A `match` arm's (or `else`'s) resolved type doesn't match the others
    /// -- the `match` analogue of `IfBranchTypeMismatch`.
    MatchArmTypeMismatch { expected: ResolvedType, found: ResolvedType },

    // -- mutability --
    /// A binding not declared `mut` was used somewhere that requires write
    /// access to it: an assignment, `++`/`--`, an explicit `&mut`, or the
    /// implicit `mut self` auto-ref a mutating method call needs. `ident`
    /// names the binding itself; the requiring expression's own span is
    /// what this anchors to (the assignment, the `&mut`, the call, ...).
    NotMutableBinding { ident: Ident },
    /// Same requirement as `NotMutableBinding`, reached through a pointer
    /// instead -- the *pointer's* type would need to be `*mut T`, not `*T`
    /// (a `*T` pointer stays unwritable no matter how the binding holding
    /// it was declared).
    NotMutablePointer,

    // -- unions --
    /// `Union { }` -- a union literal setting no field at all; unlike a
    /// struct, there's no "every field" to zero-init, and unlike an enum
    /// variant, there's no tag to pick a default from -- exactly one field
    /// must be named so the write actually has a well-defined shape.
    UnionLiteralMissingField { r#union: Ident },
    /// `Union { a = 1; b = 2; }` -- a union literal setting more than one
    /// field; they'd overlap the same storage, so only one write is ever
    /// meaningful. `fields` lists every field name that was set, in source
    /// order.
    UnionLiteralTooManyFields { r#union: Ident, fields: Vec<Ident> },

    // -- casting --
    /// `<Type>expr` where either side isn't castable at all -- scoped to
    /// numeric types and pointers (see `ResolvedType::cast_class`);
    /// structs/enums/unions/slices/`bool`/`char` have no cast support.
    InvalidCast { from: ResolvedType, to: ResolvedType },
    /// `<*mut T>expr` where `expr`'s own pointer type is immutable (`*T`,
    /// not `*mut T`) -- the same directional rule `ResolvedType::accepts`
    /// already applies to pointer coercion, checked here at a cast site
    /// instead of a call/assignment site.
    CastToMutablePointer { from: ResolvedType, to: ResolvedType },

    // -- overload resolution --
    /// A call (or a bare, uncalled reference) to an overloaded name where
    /// no candidate's parameters accept the arguments given -- `candidates`
    /// lists every overload's signature, so the message can show what
    /// *would* have matched.
    NoMatchingOverload { name: Ident, candidates: Vec<ResolvedFunctionType> },
    /// A call (or a bare, uncalled reference) to an overloaded name where
    /// two or more candidates are equally good a match -- see
    /// `Analyzer::resolve_overload`'s scoring rule for what "equally good"
    /// means (fewest literal arguments needing a non-default type).
    /// `candidates` lists every *tied* candidate.
    AmbiguousOverload { name: Ident, candidates: Vec<ResolvedFunctionType> },
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
            Self::UndefinedPathHead { name, similar_module, similar_type } => {
                let mut d = d.with_label(span, "not a known module or struct");
                if let Some(similar) = similar_type {
                    d = d.with_help(format!("a type with a similar name exists: `{}`", similar.as_ref()));
                }
                if let Some(similar) = similar_module {
                    d = d.with_help(format!(
                        "an imported module with a similar name exists: `{}`",
                        similar.as_ref()
                    ));
                }
                if similar_type.is_none() && similar_module.is_none() {
                    d = d.with_note(format!(
                        "if `{}` is a module, it must be imported first: `import {};`",
                        name.as_ref(),
                        name.as_ref()
                    ));
                }
                d
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
            Self::CompoundAssignTargetNotAPlace => d
                .with_label(span, "cannot assign to this expression")
                .with_note("only variables, fields, indexes, and dereferences can be assigned to"),
            Self::AssignmentTypeMismatch { target, value } => {
                let d = d
                    .with_label(span, format!("expected `{target}`, found `{value}`"))
                    .with_note("Omega has no implicit conversions; the value must have exactly the target's type");
                // Both sides being *refined* variants of one enum means the
                // variable was `:=`-inferred to one specific variant --
                // declaring it as the plain enum is exactly what holds any.
                match (target, value) {
                    (
                        ResolvedType::Enum { cell: expected, variant: Some(_) },
                        ResolvedType::Enum { cell: found, variant: Some(_) },
                    ) if expected.borrow().id == found.borrow().id => d.with_help(format!(
                        "declare the variable with the plain enum type to hold any variant: `name : {} = ...;`",
                        expected.borrow().name.as_ref()
                    )),
                    _ => d,
                }
            }
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
            Self::InvalidBitNotOperand { r#type } => d
                .with_label(span, format!("this has type `{}`", r#type))
                .with_note("unary `~` requires a signed or unsigned integer"),
            Self::FloatBitwiseOperand => d
                .with_label(span, "bitwise/shift operators require integer operands")
                .with_note("there is no native float bitwise/shift instruction"),
            Self::NotSliceable { found } => d
                .with_label(span, format!("this has type `{found}`, which cannot be sliced"))
                .with_note("only sized arrays (`[T; N]`) and slices (`*[T]`) support `[start..end]`"),
            Self::SliceRequiresAddressOf => d
                .with_label(span, "a slice expression must be prefixed with `&` or `&mut`")
                .with_note("write `&base[start..end]` for an immutable slice, or `&mut base[start..end]` for a mutable one"),
            Self::ImmutableSliceSource => d
                .with_label(span, "cannot take a mutable slice of an immutable slice")
                .with_note("this slice value is immutable, regardless of whether the binding holding it is `mut`"),
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
            Self::DeferInsideLoopNotSupported => d
                .with_label(span, "`defer` cannot appear inside a loop body")
                .with_help("move the `defer` outside the loop, or run the cleanup code directly"),
            Self::ReturnInsideDefer => d
                .with_label(span, "cannot `return` from inside a `defer` body")
                .with_note("deferred code runs while the function is already returning"),
            Self::NestedDeferNotSupported => d
                .with_label(span, "`defer` cannot appear inside another `defer` body")
                .with_note("a defer's body already runs exactly once, at function exit"),
            Self::StructLiteralNotAStruct { found } => d
                .with_label(span, format!("`{found}` is not a struct or union"))
                .with_note("only struct and union types can be built with `Name { field = value; ... }`"),
            Self::DuplicateFieldInitializer { field, previous } => d
                .with_label(span, format!("`{}` set again here", field.as_ref()))
                .with_secondary_label(*previous, format!("`{}` first set here", field.as_ref())),
            Self::FieldTypeMismatch { expected, found, .. } => d
                .with_label(span, format!("expected `{expected}`, found `{found}`"))
                .with_note("Omega has no implicit conversions; each value must have exactly its field's type"),
            Self::MissingFieldInitializers { r#struct, missing } => d
                .with_label(span, format!("missing {}", field_list(missing)))
                .with_note(format!(
                    "a struct literal must set every field of `{}`; there is no implicit zero-initialization",
                    r#struct.as_ref()
                )),
            Self::NoSuchStructFunction { r#struct, similar, .. } => {
                let d = d.with_label(span, format!("not found in `{}`", r#struct.as_ref()));
                match similar {
                    Some(name) => {
                        d.with_help(format!("a function with a similar name exists: `{}`", name.as_ref()))
                    }
                    None => d,
                }
            }
            Self::MemberFunctionWithoutInstance { r#struct, function } => d
                .with_label(span, "this function takes `self`")
                .with_help(format!(
                    "call it on a value of type `{}` instead: `value.{}(...)`",
                    r#struct.as_ref(),
                    function.as_ref()
                )),
            Self::StaticFunctionOnInstance { r#struct, function } => d
                .with_label(span, "this function does not take `self`")
                .with_help(format!(
                    "call it through the type's name instead: `{}::{}(...)`",
                    r#struct.as_ref(),
                    function.as_ref()
                )),
            Self::StaticAccessOnNonStruct { .. } => {
                d.with_label(span, "only structs and enums have functions")
            }
            Self::StructPathTooDeep { .. } => {
                d.with_label(span, "a function has no items of its own")
            }
            Self::NotAModule { .. } => {
                d.with_label(span, "only modules and struct/enum types can contain items")
            }
            Self::EnumTagNotFirst => d
                .with_label(span, "`tag` must be the header's first entry")
                .with_help("move `tag: ...` to the front of the header"),
            Self::EnumTagNotInteger { found } => d
                .with_label(span, format!("`{found}` cannot be a tag type"))
                .with_note("enum tags are currently limited to integer types (i8..i64, u8..u64, isize, usize)"),
            Self::EnumHeaderFieldUnsupportedType { found, .. } => d
                .with_label(span, format!("`{found}` has no literal constant form"))
                .with_note(
                    "each variant supplies this field's value as a compile-time constant,\nso header fields are currently limited to integers, floats, bool, char, and `*u8`",
                ),
            Self::EnumVariantArgCount { expected, found, has_tag, .. } => {
                let what = if *has_tag { "the tag, then one value per header field" } else { "one value per header field" };
                d.with_label(span, format!("expected {expected} {}, found {found}", plural(*expected, "value")))
                    .with_note(format!("each variant's `(...)` must supply {what}, in header order"))
            }
            Self::EnumValueNotConstant => d
                .with_label(span, "not a literal constant")
                .with_note("a variant's tag and header values are baked in at the definition,\nso they must be literals (a number, string, bool, or char)"),
            Self::EnumValueTypeMismatch { expected, found } => {
                d.with_label(span, format!("expected `{expected}`, found {found}"))
            }
            Self::DuplicateEnumTag { value, previous_variant, previous, .. } => d
                .with_label(span, format!("tag {value} used again here"))
                .with_secondary_label(*previous, format!("first used by variant '{}'", previous_variant.as_ref()))
                .with_note("the tag is how variants are told apart at runtime, so each variant needs its own"),
            Self::EnumFieldShadowsHeader { field, .. } => {
                let d = d.with_label(span, format!("`{}` already names a header field", field.as_ref()));
                if field.as_ref() == "tag" {
                    d.with_note("`tag` is reserved: every enum value exposes its tag as `value.tag`")
                } else {
                    d.with_note("header fields and body fields are both accessed as `value.name`, so they share one namespace")
                }
            }
            Self::EnumLiteralWithoutVariant { r#enum, example } => d
                .with_label(span, "an enum value is always a specific variant")
                .with_help(format!("name the variant: `{}::{} {{ ... }}`", r#enum.as_ref(), example.as_ref())),
            Self::NoSuchEnumMember { r#enum, similar_variant, similar_function, .. } => {
                let mut d = d.with_label(span, format!("not found in `{}`", r#enum.as_ref()));
                if let Some(name) = similar_variant {
                    d = d.with_help(format!("a variant with a similar name exists: `{}`", name.as_ref()));
                }
                if let Some(name) = similar_function {
                    d = d.with_help(format!("a function with a similar name exists: `{}`", name.as_ref()));
                }
                d
            }
            Self::EnumVariantMissingBody { r#enum, variant, fields } => d
                .with_label(span, format!("variant '{}' has {}", variant.as_ref(), field_list(fields)))
                .with_help(format!(
                    "supply them with a body: `{}::{} {{ {} }}`",
                    r#enum.as_ref(),
                    variant.as_ref(),
                    fields.iter().map(|f| format!("{}: ...;", f.as_ref())).collect::<Vec<_>>().join(" ")
                )),
            Self::EnumVariantHasNoBody { r#enum, variant } => d
                .with_label(span, format!("variant '{}' declares no fields", variant.as_ref()))
                .with_help(format!("write it bare: `{}::{}`", r#enum.as_ref(), variant.as_ref())),
            Self::StructLiteralPathTooDeep { r#struct, .. } => d
                .with_label(span, format!("`{}` is a struct -- it has no variants", r#struct.as_ref()))
                .with_help(format!("build it directly: `{} {{ ... }}`", r#struct.as_ref())),
            Self::EnumFieldWrongVariant { field, owner, actual } => d
                .with_label(span, format!("this value is `{}`, which has no field '{}'", actual.as_ref(), field.as_ref()))
                .with_note(format!("'{}' belongs to variant '{}'", field.as_ref(), owner.as_ref())),
            Self::EnumFieldVariantUnknown { field, owner, .. } => d
                .with_label(span, format!("this value's variant is not statically known here"))
                .with_note(format!(
                    "'{}' belongs to variant '{}', which this value may or may not be;\nonly `tag` and the shared header fields are always present",
                    field.as_ref(),
                    owner.as_ref()
                )),
            Self::NoSuchEnumField { r#enum, similar, .. } => {
                let d = d.with_label(span, format!("`{}` has no field by that name", r#enum.as_ref()));
                match similar {
                    Some(name) => d.with_help(format!("a field with a similar name exists: `{}`", name.as_ref())),
                    None => d,
                }
            }
            Self::GenericPathTooDeep { r#type } => d
                .with_label(span, format!("nothing nests deeper than `{}`'s own members", r#type.as_ref())),
            Self::EnumFieldImmutable { field } => d
                .with_label(span, format!("`{}` is fixed by the value's variant", field.as_ref()))
                .with_note("the tag and header fields are per-variant constants; only a variant's own body fields can be assigned"),
            Self::EnumHeaderFieldInLiteral { field } => d
                .with_label(span, format!("`{}` is a header field", field.as_ref()))
                .with_note("header values are fixed per variant by the enum's definition, so a construction site never supplies them"),

            Self::PatternValueNotConstant => d
                .with_label(span, "not a literal constant")
                .with_note("a match pattern is checked against the scrutinee's whole domain at compile time,\nso its bounds must be literals (a number, bool, or char)"),
            Self::NoSuchVariantInPattern { r#enum, similar, .. } => {
                let d = d.with_label(span, format!("not found in `{}`", r#enum.as_ref()));
                match similar {
                    Some(name) => d.with_help(format!("a variant with a similar name exists: `{}`", name.as_ref())),
                    None => d,
                }
            }
            Self::PatternNotEnumVariant { r#enum } => d
                .with_label(span, format!("`{}` can only be matched by naming one of its variants", r#enum.as_ref()))
                .with_help(format!("write a pattern like `{}::SomeVariant`", r#enum.as_ref())),
            Self::PatternIsEnumVariant { r#enum, variant, scrutinee } => d.with_label(
                span,
                format!("`{}::{}` is a variant of `{}`, not of `{scrutinee}`", r#enum.as_ref(), variant.as_ref(), r#enum.as_ref()),
            ),
            Self::PatternTypeMismatch { expected, found } => {
                d.with_label(span, format!("expected `{expected}`, found `{found}`"))
            }
            Self::UnsupportedMatchScrutinee { r#type } => d
                .with_label(span, format!("cannot match on `{type}`"))
                .with_note("`match` supports enums, integers, and `bool`"),
            Self::OverlappingMatchArm { previous } => d
                .with_label(span, "this pattern is unreachable")
                .with_secondary_label(*previous, "already covered here"),
            Self::NonExhaustiveMatchEnum { missing, .. } => d
                .with_label(span, format!("missing {} {}", plural(missing.len(), "variant"), ident_list(missing)))
                .with_help("cover the remaining variants, or add an `else` block"),
            Self::NonExhaustiveMatchValue { gaps, .. } => d
                .with_label(span, format!("not covered: {}", gaps.join(", ")))
                .with_help("cover the remaining values, or add an `else` block"),
            Self::MatchArmTypeMismatch { expected, found } => d
                .with_label(span, format!("this arm produces `{found}`, but earlier arms produce `{expected}`"))
                .with_note("every arm of a `match` used as an expression must produce the same type"),

            Self::NotMutableBinding { ident } => d
                .with_label(span, format!("`{}` is not declared `mut`", ident.as_ref()))
                .with_help(format!("declare it `mut {}`", ident.as_ref())),
            Self::NotMutablePointer => d
                .with_label(span, "this pointer's pointee is immutable")
                .with_help("use `*mut T` instead of `*T`, and `&mut` to create one"),
            Self::UnionLiteralMissingField { r#union } => d
                .with_label(span, "no field set")
                .with_help(format!("set exactly one of `{}`'s fields", r#union.as_ref())),
            Self::UnionLiteralTooManyFields { r#union, fields } => d
                .with_label(span, format!("{} set, but a union literal may only set one", field_list(fields)))
                .with_help(format!("`{}`'s fields overlap the same storage; pick exactly one", r#union.as_ref())),
            Self::InvalidCast { from, to } => d
                .with_label(span, format!("no cast exists from '{from}' to '{to}'"))
                .with_note("casts are only supported between numeric types and pointers"),
            Self::CastToMutablePointer { from, to } => d
                .with_label(span, format!("cannot cast '{from}' to '{to}'"))
                .with_help("a pointer cast can only target `*mut T` if the source is already `*mut`"),
            Self::NoMatchingOverload { name, candidates } => {
                let mut d = d.with_label(span, format!("no overload of `{}` matches this", name.as_ref()));
                for candidate in candidates {
                    d = d.with_note(format!("candidate: {}", ResolvedType::Function(candidate.clone())));
                }
                d
            }
            Self::AmbiguousOverload { name, candidates } => {
                let mut d = d.with_label(span, format!("reference to `{}` is ambiguous", name.as_ref()));
                for candidate in candidates {
                    d = d.with_note(format!("candidate: {}", ResolvedType::Function(candidate.clone())));
                }
                d
            }
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
        TypeResolutionError::NoSuchVariantForType { r#enum, similar, .. } => {
            let d = d.with_label(span, format!("not found in `{}`", r#enum.as_ref()));
            match similar {
                Some(name) => d.with_help(format!("a variant with a similar name exists: `{}`", name.as_ref())),
                None => d,
            }
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

/// `'a'` / `'a' and 'b'` / `'a', 'b', and 'c'` -- the bare listing
/// `field_list`/`NonExhaustiveMatchEnum`'s diagnostic build their own
/// noun-prefixed message around.
fn ident_list(names: &[Ident]) -> String {
    let names: Vec<String> = names.iter().map(|f| format!("'{}'", f.as_ref())).collect();
    match names.as_slice() {
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
        [] => String::new(),
    }
}

/// `field 'a'` / `fields 'a' and 'b'` / `fields 'a', 'b', and 'c'` -- for
/// `MissingFieldInitializers`' headline and label.
fn field_list(fields: &[Ident]) -> String {
    format!("{} {}", plural(fields.len(), "field"), ident_list(fields))
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
            Self::UndefinedPathHead { name, .. } => write!(f, "cannot find '{}' in this scope", name.as_ref()),
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
            Self::CompoundAssignTargetNotAPlace => {
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
            Self::InvalidBitNotOperand { r#type } => write!(
                f,
                "cannot apply '~' to a value of type '{}'", r#type
            ),
            Self::FloatBitwiseOperand => {
                write!(f, "bitwise/shift operators are not supported on floating-point operands")
            }
            Self::NotSliceable { found } => {
                write!(f, "cannot slice a value of type '{found}'")
            }
            Self::SliceRequiresAddressOf => {
                write!(f, "a slice expression must be prefixed with '&' or '&mut'")
            }
            Self::ImmutableSliceSource => {
                write!(f, "cannot take a mutable slice of an immutable slice")
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
            Self::DeferInsideLoopNotSupported => {
                write!(f, "'defer' is not supported inside a loop body")
            }
            Self::ReturnInsideDefer => write!(f, "'return' is not supported inside a 'defer' body"),
            Self::NestedDeferNotSupported => write!(f, "'defer' is not supported inside another 'defer' body"),
            Self::StructLiteralNotAStruct { found } => {
                write!(f, "cannot build a value of type '{found}' with a struct literal")
            }
            Self::DuplicateFieldInitializer { field, .. } => {
                write!(f, "field '{}' is set more than once", field.as_ref())
            }
            Self::FieldTypeMismatch { field, expected, found } => write!(
                f,
                "mismatched types: field '{}' is '{expected}', found '{found}'",
                field.as_ref()
            ),
            Self::MissingFieldInitializers { r#struct, missing } => {
                write!(f, "missing {} in initializer of '{}'", field_list(missing), r#struct.as_ref())
            }
            Self::NoSuchStructFunction { r#struct, function, .. } => {
                write!(f, "no function '{}' on '{}'", function.as_ref(), r#struct.as_ref())
            }
            Self::MemberFunctionWithoutInstance { r#struct, function } => write!(
                f,
                "'{}::{}' is a member function and cannot be called without an instance",
                r#struct.as_ref(),
                function.as_ref()
            ),
            Self::StaticFunctionOnInstance { r#struct, function } => write!(
                f,
                "'{}::{}' is a static function and cannot be called on an instance",
                r#struct.as_ref(),
                function.as_ref()
            ),
            Self::StaticAccessOnNonStruct { found } => {
                write!(f, "type '{found}' has no functions")
            }
            Self::EnumTagNotFirst => {
                write!(f, "the 'tag' header entry must come first")
            }
            Self::EnumTagNotInteger { found } => {
                write!(f, "enum tags must be integers, but this tag is declared as '{found}'")
            }
            Self::EnumHeaderFieldUnsupportedType { field, .. } => {
                write!(f, "header field '{}' has a type that cannot hold a constant", field.as_ref())
            }
            Self::EnumVariantArgCount { variant, expected, found, .. } => {
                write!(
                    f,
                    "variant '{}' must supply {expected} {}, but supplies {found}",
                    variant.as_ref(),
                    plural(*expected, "value")
                )
            }
            Self::EnumValueNotConstant => {
                write!(f, "enum variant values must be literal constants")
            }
            Self::EnumValueTypeMismatch { expected, found } => {
                write!(f, "mismatched types: expected '{expected}', found {found}")
            }
            Self::DuplicateEnumTag { variant, value, previous_variant, .. } => {
                write!(
                    f,
                    "variants '{}' and '{}' share the tag value {value}",
                    previous_variant.as_ref(),
                    variant.as_ref()
                )
            }
            Self::EnumFieldShadowsHeader { field, variant } => match variant {
                Some(variant) => write!(
                    f,
                    "field '{}' of variant '{}' collides with a header field",
                    field.as_ref(),
                    variant.as_ref()
                ),
                None => write!(f, "header field '{}' is declared more than once", field.as_ref()),
            },
            Self::EnumLiteralWithoutVariant { r#enum, .. } => {
                write!(f, "cannot build enum '{}' without naming a variant", r#enum.as_ref())
            }
            Self::NoSuchEnumMember { r#enum, name, .. } => {
                write!(f, "no variant or function '{}' on enum '{}'", name.as_ref(), r#enum.as_ref())
            }
            Self::EnumVariantMissingBody { r#enum, variant, .. } => {
                write!(f, "variant '{}::{}' has fields that must be initialized", r#enum.as_ref(), variant.as_ref())
            }
            Self::EnumVariantHasNoBody { r#enum, variant } => {
                write!(f, "variant '{}::{}' has no fields to initialize", r#enum.as_ref(), variant.as_ref())
            }
            Self::StructLiteralPathTooDeep { r#struct, name } => {
                write!(f, "'{}' is a struct, so '{}' cannot be one of its variants", r#struct.as_ref(), name.as_ref())
            }
            Self::EnumFieldWrongVariant { field, owner, .. } => {
                write!(f, "field '{}' belongs to a different variant ('{}')", field.as_ref(), owner.as_ref())
            }
            Self::EnumFieldVariantUnknown { field, r#enum, .. } => {
                write!(f, "cannot access variant field '{}' on a value whose '{}' variant is unknown", field.as_ref(), r#enum.as_ref())
            }
            Self::NoSuchEnumField { field, r#enum, .. } => {
                write!(f, "no field '{}' on enum '{}'", field.as_ref(), r#enum.as_ref())
            }
            Self::GenericPathTooDeep { r#type } => {
                write!(f, "path continues too far past '{}'", r#type.as_ref())
            }
            Self::EnumFieldImmutable { field } => {
                write!(f, "cannot assign to '{}' of an enum value", field.as_ref())
            }
            Self::EnumHeaderFieldInLiteral { field } => {
                write!(f, "header field '{}' cannot be initialized at a construction site", field.as_ref())
            }
            Self::StructPathTooDeep { r#struct, function } => {
                write!(f, "'{}::{}' is a function; there is nothing to look up inside it",
                    r#struct.as_ref(), function.as_ref())
            }
            Self::NotAModule { name } => {
                write!(f, "'{}' is a value, not a module or type", name.as_ref())
            }
            Self::PatternValueNotConstant => write!(f, "match patterns must be literal constants"),
            Self::NoSuchVariantInPattern { r#enum, name, .. } => {
                write!(f, "no variant '{}' on enum '{}'", name.as_ref(), r#enum.as_ref())
            }
            Self::PatternNotEnumVariant { r#enum } => {
                write!(f, "'{}' can only be matched by naming a variant", r#enum.as_ref())
            }
            Self::PatternIsEnumVariant { r#enum, variant, scrutinee } => write!(
                f,
                "mismatched types: expected '{scrutinee}', found '{}::{}'",
                r#enum.as_ref(),
                variant.as_ref()
            ),
            Self::PatternTypeMismatch { expected, found } => {
                write!(f, "mismatched types: expected '{expected}', found '{found}'")
            }
            Self::UnsupportedMatchScrutinee { r#type } => {
                write!(f, "cannot match on a value of type '{type}'")
            }
            Self::OverlappingMatchArm { .. } => write!(f, "unreachable match arm"),
            Self::NonExhaustiveMatchEnum { r#enum, .. } => {
                write!(f, "match on '{}' does not cover every variant", r#enum.as_ref())
            }
            Self::NonExhaustiveMatchValue { r#type, .. } => {
                write!(f, "match on '{type}' does not cover every value")
            }
            Self::MatchArmTypeMismatch { .. } => write!(f, "'match' arms have incompatible types"),
            Self::NotMutableBinding { ident } => write!(f, "cannot mutate '{}': not declared 'mut'", ident.as_ref()),
            Self::NotMutablePointer => write!(f, "cannot mutate through an immutable pointer"),
            Self::UnionLiteralMissingField { r#union } => {
                write!(f, "union literal for '{}' sets no field", r#union.as_ref())
            }
            Self::UnionLiteralTooManyFields { r#union, .. } => {
                write!(f, "union literal for '{}' sets more than one field", r#union.as_ref())
            }
            Self::InvalidCast { from, to } => write!(f, "cannot cast '{from}' to '{to}'"),
            Self::CastToMutablePointer { from, to } => {
                write!(f, "cannot cast '{from}' to '{to}': target is a mutable pointer")
            }
            Self::NoMatchingOverload { name, .. } => write!(f, "no overload of '{}' matches this call", name.as_ref()),
            Self::AmbiguousOverload { name, .. } => write!(f, "ambiguous reference to overloaded '{}'", name.as_ref()),
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
