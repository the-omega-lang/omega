//! What can go wrong during analysis, and how each reads as one line.

use super::*;

#[derive(Debug, Clone)]
pub enum AnalysisErrorKind {
    UnresolvedType(TypeResolutionError),
    /// An unqualified name that resolves to nothing visible. `similar` is a
    /// close-enough visible name, when one exists.
    UndefinedVariable {
        name: Ident,
        similar: Option<Ident>,
    },
    /// A qualified place/value path (`head::rest`) whose head names nothing
    /// visible: not an imported module alias, not a type, and not one of
    /// this module's own items. What the user *meant* can't be known (a
    /// module they forgot to import, or a typo'd struct name), so this
    /// carries a "did you mean" candidate from each world and only ever
    /// suggests what actually exists.
    UndefinedPathHead {
        name: Ident,
        similar_module: Option<Ident>,
        similar_type: Option<Ident>,
    },
    /// A field access on something that isn't a struct (after auto-deref).
    NotAStruct {
        found: ResolvedType,
    },
    /// A field access naming a field `base` doesn't have.
    NoSuchField {
        field: Ident,
        base: ResolvedType,
    },
    /// A field access (or struct/union/enum-variant literal initializer)
    /// naming a field that exists on `base` but isn't visible from this
    /// module -- `field` is hidden (or `internal` to a different package)
    /// relative to the accessing site. Bypassed by `reveal` (see
    /// `Analyzer::check_visibility`).
    FieldNotVisible {
        field: Ident,
        base: ResolvedType,
    },
    /// A method call resolving to a method that exists on `base` but isn't
    /// visible from this module -- same rule as `FieldNotVisible`, for
    /// methods instead of data fields.
    MethodNotVisible {
        method: Ident,
        base: ResolvedType,
    },
    /// An index projection on something that isn't an array/slice.
    NotAnArray {
        found: ResolvedType,
    },
    /// A call supplying the wrong number of arguments (too many *or* too
    /// few -- despite this once being named `TooManyArguments`).
    WrongArgumentCount {
        expected: usize,
        found: usize,
    },
    ArgumentTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    UnresolvedCallee,
    InvalidNumberType(Ident),
    UnresolvedInnerExpression,
    /// A name is declared twice in the same scope (a second parameter with
    /// the same name, or a second local `ident: type;` in the same function
    /// body). Shadowing an *outer* scope is fine and doesn't trigger this.
    /// `previous` is the first declaration's span, when the declaring site
    /// tracks one -- rendered as a "first declared here" secondary label.
    Redeclaration {
        name: Ident,
        previous: Option<Span>,
    },
    /// An assignment's left-hand side isn't syntactically a place (e.g.
    /// `5 = 3;`) -- rejected here so `CheckedAssignment.target` can be typed
    /// as `CheckedPlace` rather than a general expression.
    AssignmentTargetNotAPlace,
    /// An assignment's value doesn't have the same resolved type as its
    /// target (e.g. assigning a pointer into an `i32` local).
    AssignmentTypeMismatch {
        target: ResolvedType,
        value: ResolvedType,
    },
    /// A compound assignment's (`+= -= *= /= %= &= |= ^= <<= >>=`)
    /// left-hand side isn't syntactically a place -- same reasoning as
    /// `AssignmentTargetNotAPlace`.
    CompoundAssignTargetNotAPlace,
    /// A number literal doesn't fit in its resolved type.
    NumberLiteralOutOfRange {
        literal: String,
        r#type: ResolvedType,
    },
    /// `*expr` where `expr`'s resolved type isn't a pointer.
    NotAPointer {
        found: ResolvedType,
    },
    /// `&expr` where `expr` isn't syntactically a place (e.g. `&5`).
    AddressOfNotAPlace,
    /// A `+ - * / %` operand isn't numeric.
    InvalidBinaryOperand {
        op: BinaryOp,
        r#type: ResolvedType,
    },
    /// Arithmetic and bitwise operators have no meaning for Unicode scalar
    /// values. `char` remains comparable, but its codepoint must be cast
    /// explicitly before arithmetic.
    CharArithmeticNotAllowed {
        op: String,
    },
    /// Two pointers may be compared or subtracted for their byte distance;
    /// every other pointer-pair arithmetic operation is meaningless.
    PointerPairArithmetic {
        op: BinaryOp,
    },
    /// A unary `-` operand isn't a signed integer or float.
    InvalidNegateOperand {
        r#type: ResolvedType,
    },
    /// A unary `~` operand isn't a signed or unsigned integer.
    InvalidBitNotOperand {
        r#type: ResolvedType,
    },
    /// A `& | ^ << >>` operand is a float -- there's no native instruction
    /// for any of these on floating-point operands.
    FloatBitwiseOperand,
    /// `base[start..end]` where `base`'s resolved type is neither
    /// `SizedArray` nor `Slice`.
    NotSliceable {
        found: ResolvedType,
    },
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
    InvalidSliceBound {
        r#type: ResolvedType,
    },
    /// `&arr[start..]` (an open-ended range) on a `*[]T` base -- unlike
    /// `SizedArray`/`Slice`/`Str`, it carries no length anywhere to default
    /// a missing end to.
    MissingSliceEnd,
    /// `&comp_arr_binding[range]` -- a `comp`-bound `*[]T` has no
    /// established const-promotion story (see `Analyzer::analyze_slice`'s
    /// own comment on this); narrow and likely never hit in practice, but
    /// rejected explicitly rather than silently mishandled.
    CompPointerSliceNotSupported,
    /// Bare `..` has no type source outside an index or pattern.
    RangeNotAllowedHere,
    RangeNeedsBounded { r#type: ResolvedType },
    /// `[]` -- there's no element to infer the array's item type from.
    EmptyArrayLiteral,
    /// An array literal's elements don't all share the same resolved type
    /// (the first element's type is what every other element is checked
    /// against).
    ArrayElementTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    /// `ident : []T = value;` where `value` isn't an array literal (or
    /// analyzed to a different item type than `T`) -- there's nothing to
    /// infer the real length from.
    ArraySizeNotInferable,
    /// `&mut [...]` -- a compile-time slice is always immutable, just like
    /// a string literal (see `ConstValue::Slice`).
    ConstSliceCannotBeMutable,
    /// An element of a `&[...]` compile-time slice isn't a literal constant
    /// (the sibling of `EnumValueNotConstant`, worded for ordinary
    /// expression position rather than an enum header value -- see
    /// `Analyzer::const_eval_slice`'s doc comment for why these stay
    /// separate).
    ConstSliceElementNotConstant,
    /// The sibling of `EnumValueTypeMismatch`, for a `&[...]` element.
    ConstSliceElementTypeMismatch {
        expected: ResolvedType,
        found: String,
    },
    /// A `+ - * / %` operand's types don't match each other (e.g. `i32 +
    /// i64`) -- unlike `InvalidBinaryOperand`, both operands *are* numeric,
    /// they just aren't the same numeric type; this language has no implicit
    /// numeric conversions, so a mismatch here is always an error rather than
    /// a promotion. The per-operand spans let the diagnostic point at each
    /// side with its own type.
    BinaryOperandTypeMismatch {
        left: ResolvedType,
        left_span: Span,
        right: ResolvedType,
        right_span: Span,
    },
    /// `%` (`BinaryOp::Rem`) applied to a float operand -- there's no native
    /// floating-point remainder instruction to lower this to (matching C,
    /// which requires calling `fmod`/`fmodf` instead of using `%`).
    FloatRemainder,
    /// An `if`/`while`/`for` condition doesn't resolve to `Bool`.
    NonBoolCondition {
        r#type: ResolvedType,
    },
    /// An `if`/`else if`/`else` branch's resolved type doesn't match the
    /// others (see `Analyzer::block_type`/the `HirExpr::If` arm for exactly
    /// how "the others" is determined, including how a branch that diverges
    /// via `return` is exempt).
    IfBranchTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    /// A function's body doesn't produce its declared return type -- neither
    /// a tail expression of the right type, nor an unconditional trailing
    /// `return`, nor (for `Void`) falling off the end with no tail at all.
    /// Also used for an individual `return <expr>;` whose type doesn't match
    /// the enclosing function's declared return type.
    ReturnTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    /// `++expr`/`--expr` where `expr` isn't syntactically a place (e.g.
    /// `++5`).
    IncrementTargetNotAPlace,
    /// `++expr`/`--expr` where `expr`'s resolved type isn't numeric (e.g.
    /// `bool`, `char`, or a pointer).
    InvalidIncrementOperand {
        r#type: ResolvedType,
    },
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
    /// A macro's public interface is wider than an item it references from
    /// its body. The item must be at least as visible as the macro.
    MacroDependencyTooPrivate {
        item: Ident,
        macro_visibility: omega_parser::prelude::Visibility,
        item_visibility: omega_parser::prelude::Visibility,
    },
    /// A qualified path resolved to a type (a struct), not a value, in a
    /// position that requires a value (e.g. calling it, or using it as a
    /// place).
    NotAValue(Vec<Ident>),
    /// A generic function call's argument-driven type inference
    /// (`Analyzer::resolve_generic_call`) couldn't deduce a concrete type
    /// for this declared generic parameter -- it never appeared (in a
    /// structurally recognizable position) in any of the call's arguments.
    UnresolvedGenericParam(Ident),
    /// The specific "couldn't infer" case whose cause deserves teaching:
    /// `f<T>(x: *T)` called with a fat pointer (`*[]u8`/`*str`). A `Slice`/
    /// `Str` carries a runtime length, so it can never match the thin
    /// pointer `*T` -- and there is no `[]T` type for `T` to bind to (`[]T`
    /// is not valid on its own). This is not an inference gap; the rule is
    /// the point (a generic that must accept slices takes `x: T` by value,
    /// which binds `T = *[]u8`, or spells the slice out as `x: *[]T`).
    GenericParamFromFatPointer {
        parameter: Ident,
        found: ResolvedType,
    },
    /// A generic struct/union/enum-variant literal (`Name { field = value;
    /// ... }`), or a bare enum unit-variant reference (`Enum::Variant`),
    /// was written with no explicit `<...>` type arguments, and neither the
    /// literal's own field values nor an available `expected` (surrounding
    /// context) type pinned down every one of `r#type`'s declared generic
    /// parameters -- `generics` names whichever ones are still unresolved.
    UnresolvedLiteralGeneric {
        r#type: Ident,
        generics: Vec<Ident>,
    },
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
    StructLiteralNotAStruct {
        found: ResolvedType,
    },
    /// A struct literal setting the same field twice. `previous` is the
    /// first initializer's span -- rendered as a "first set here" label.
    DuplicateFieldInitializer {
        field: Ident,
        previous: Span,
    },
    /// A struct literal field's value doesn't have the field's declared
    /// type.
    FieldTypeMismatch {
        field: Ident,
        expected: ResolvedType,
        found: ResolvedType,
    },
    /// A struct literal that doesn't cover every declared field -- partial
    /// initialization is not allowed (there is no implicit zeroing).
    MissingFieldInitializers {
        r#struct: Ident,
        missing: Vec<Ident>,
    },
    /// `Struct::function` naming a function `Struct` doesn't have. `similar`
    /// is a close-enough function name on that struct, when one exists.
    NoSuchStructFunction {
        r#struct: Ident,
        function: Ident,
        similar: Option<Ident>,
    },
    /// `Struct::function(...)` where `function` takes `self` -- a member
    /// function needs an instance to be called on.
    MemberFunctionWithoutInstance {
        r#struct: Ident,
        function: Ident,
    },
    /// `value.function(...)` where `function` does *not* take `self` -- a
    /// static function is called through the struct's name, not an instance.
    StaticFunctionOnInstance {
        r#struct: Ident,
        function: Ident,
    },
    /// `Type::name` where `Type` is a real type but not a struct (e.g.
    /// `i32::something`) -- only structs can have functions.
    StaticAccessOnNonStruct {
        found: ResolvedType,
    },
    /// `Struct::function::more` -- a path trying to reach *through* a
    /// struct's function; functions have no items of their own.
    StructPathTooDeep {
        r#struct: Ident,
        function: Ident,
    },
    /// `head::item` where `head` names a *value* (a function or global) --
    /// values have no items of their own; only modules and struct types do.
    NotAModule {
        name: Ident,
    },
    /// An enum header entry named `tag` that isn't the *first* entry -- the
    /// tag is required to lead the header (it's how the runtime layout
    /// starts, and how the compiler tells variants apart).
    EnumTagNotFirst,
    /// An explicit tag (`tag: T` leading the header) whose `T` isn't an
    /// integer type -- tags are currently always numeric.
    EnumTagNotInteger {
        found: ResolvedType,
    },
    /// A header field whose type has no compile-time-constant literal form
    /// (a struct, an array, ...) -- header values are per-variant constants,
    /// so every header field must be expressible as one.
    EnumHeaderFieldUnsupportedType {
        field: Ident,
        found: ResolvedType,
    },
    /// A variant supplying the wrong number of header values. `expected`
    /// counts the explicit tag when the enum declares one (`has_tag`), so
    /// the message can spell out what the list must contain.
    EnumVariantArgCount {
        variant: Ident,
        expected: usize,
        found: usize,
        has_tag: bool,
    },
    /// A variant's tag/header value that isn't a literal constant -- the
    /// header is per-variant *constant* data, baked in at the definition.
    EnumValueNotConstant,
    /// A variant's tag/header value whose literal kind can't be a value of
    /// the field's declared type (e.g. a string where `u32` is expected).
    /// `found` is a short description of what was written.
    EnumValueTypeMismatch {
        expected: ResolvedType,
        found: String,
    },
    /// Two variants sharing one tag value -- tags are how variants are told
    /// apart at runtime, so they must be unique per variant.
    DuplicateEnumTag {
        variant: Ident,
        value: String,
        previous_variant: Ident,
        previous: Span,
    },
    /// A name already claimed elsewhere in the same enum's shared
    /// `value.name` namespace -- the tag, a header field, a shared dynamic
    /// field, and (when `variant` is `Some`) that variant's own body
    /// fields all draw from one namespace, so none of them may collide
    /// with any other. `variant` is `None` for a definition-time
    /// collision among the tag/header/dynamic fields themselves (which
    /// apply enum-wide), `Some` for a variant's own body field colliding
    /// with one of those.
    EnumFieldNameCollision {
        field: Ident,
        variant: Option<Ident>,
    },
    /// `Enum { ... }` -- an enum can't be built by naming just the enum; a
    /// specific variant must be chosen. `example` is a real variant of this
    /// enum, for the help text.
    EnumLiteralWithoutVariant {
        r#enum: Ident,
        example: Ident,
    },
    /// `Enum::Name`/`Enum::Name { ... }` where `Name` is neither a variant
    /// nor a function of the enum. Carries a "did you mean" candidate from
    /// each namespace; only ever suggests what actually exists.
    NoSuchEnumMember {
        r#enum: Ident,
        name: Ident,
        similar_variant: Option<Ident>,
        similar_function: Option<Ident>,
    },
    /// `Enum::Variant` (bare, no `{ ... }`) where the variant declares body
    /// fields -- they'd be left uninitialized, and there is no implicit
    /// zeroing anywhere in this language.
    EnumVariantMissingBody {
        r#enum: Ident,
        variant: Ident,
        fields: Vec<Ident>,
    },
    /// `Enum::Variant { ... }` where the variant declares *no* body fields.
    EnumVariantHasNoBody {
        r#enum: Ident,
        variant: Ident,
    },
    /// `Struct::Name { ... }` -- a literal path reaching into a struct as
    /// if it had variants.
    StructLiteralPathTooDeep {
        r#struct: Ident,
        name: Ident,
    },
    /// A field access naming a *body* field of a different variant than the
    /// one this value statically is.
    EnumFieldWrongVariant {
        field: Ident,
        owner: Ident,
        actual: Ident,
    },
    /// A field access naming a body field on an enum value whose variant
    /// isn't statically known -- without knowing the variant, the field may
    /// not exist in the value at all. `owner` is the variant declaring it.
    EnumFieldVariantUnknown {
        field: Ident,
        r#enum: Ident,
        owner: Ident,
    },
    /// A field access naming something that is neither the tag, a header
    /// field, a shared dynamic field, nor any variant's body field.
    NoSuchEnumField {
        field: Ident,
        r#enum: Ident,
        similar: Option<Ident>,
    },
    /// A path with explicit generic arguments (`Optional<u32>::...`)
    /// continuing more than one segment past the instantiated type --
    /// nothing nests deeper than a type's own members.
    GenericPathTooDeep {
        r#type: Ident,
    },
    /// An assignment to an enum value's tag or one of its header fields --
    /// both are per-variant constants; only a variant's own body fields and
    /// shared dynamic fields are mutable.
    EnumFieldImmutable {
        field: Ident,
    },
    /// A variant-body literal (`Enum::Variant { ... }`) trying to set a
    /// *header* field -- header values are fixed per variant by the enum's
    /// own definition, never supplied at a construction site.
    EnumHeaderFieldInLiteral {
        field: Ident,
    },

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
    NoSuchVariantInPattern {
        r#enum: Ident,
        name: Ident,
        similar: Option<Ident>,
    },
    /// A value/range pattern (`100`, `0..<10`) matched against an enum
    /// scrutinee -- an enum can only be matched by naming one of its
    /// variants.
    PatternNotEnumVariant {
        r#enum: Ident,
    },
    /// An `Enum::Variant` pattern matched against a non-enum scrutinee.
    PatternIsEnumVariant {
        r#enum: Ident,
        variant: Ident,
        scrutinee: ResolvedType,
    },
    /// A value/range pattern's own type doesn't match the scrutinee's exact
    /// type (e.g. a `u32` scrutinee matched against an `i32` literal --
    /// this language has no implicit numeric conversions anywhere else
    /// either).
    PatternTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    /// `match`'s scrutinee isn't a supported type -- scoped to enums,
    /// integers, and `bool` for now (see `ResolvedType::integer_domain`).
    UnsupportedMatchScrutinee {
        r#type: ResolvedType,
    },
    /// Two arms' patterns cover the same value -- an enum variant named by
    /// more than one arm, or two numeric/`bool` patterns whose intervals
    /// intersect. `previous` is whichever of the two was written *first*;
    /// the error itself is anchored on the other.
    OverlappingMatchArm {
        previous: Span,
    },
    /// An enum `match` covers only some variants and has no `else` --
    /// `missing` lists every variant left uncovered.
    NonExhaustiveMatchEnum {
        r#enum: Ident,
        missing: Vec<Ident>,
    },
    /// A numeric/`bool` `match` doesn't cover its scrutinee's whole domain
    /// and has no `else` -- `gaps` describes each uncovered sub-range.
    NonExhaustiveMatchValue {
        r#type: ResolvedType,
        gaps: Vec<String>,
    },
    /// A bare `..` catch-all arm on a numeric/`bool`/`char` match, but
    /// what's left uncovered by every other arm isn't exactly one
    /// contiguous range -- `gaps` (always 2 or more here; the zero-gap
    /// case is `CatchAllPatternRedundant` instead) is how many disjoint
    /// sub-ranges remain, genuinely ambiguous: `..` can't be stretched
    /// across a hole. See `RangeExpr::is_catch_all`.
    CatchAllRangeNotInferable {
        gaps: usize,
    },
    /// A bare `..` catch-all arm with nothing left for it to cover -- every
    /// other arm (numeric/`bool`/`char` range, or enum variant) already
    /// exhaustively covers the scrutinee on its own.
    CatchAllPatternRedundant,
    /// A second bare `..` arm in the same `match` -- there's only one
    /// "everything else" to have, and no principled way to split it
    /// between two catch-alls.
    MultipleCatchAllPatterns {
        previous: Span,
    },
    /// A `match` arm's (or `else`'s) resolved type doesn't match the others
    /// -- the `match` analogue of `IfBranchTypeMismatch`.
    MatchArmTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },

    // -- mutability --
    /// A binding not declared `mut` was used somewhere that requires write
    /// access to it: an assignment, `++`/`--`, an explicit `&mut`, or the
    /// implicit `mut self` auto-ref a mutating method call needs. `ident`
    /// names the binding itself; the requiring expression's own span is
    /// what this anchors to (the assignment, the `&mut`, the call, ...).
    NotMutableBinding {
        ident: Ident,
    },
    /// Same requirement as `NotMutableBinding`, reached through a pointer
    /// instead -- the *pointer's* type would need to be `*mut T`, not `*T`
    /// (a `*T` pointer stays unwritable no matter how the binding holding
    /// it was declared).
    NotMutablePointer,
    /// A write to something that is not a place at all: the mutation would
    /// land in a freshly-produced temporary that is immediately discarded.
    /// Reachable from both shapes that produce a non-place root -- a
    /// `*mut self` requirement against an rvalue receiver
    /// (`Bump::bump(make())`) and an ordinary projected write through one
    /// (`make().n = 5`) -- so its wording deliberately names neither a
    /// receiver nor `*mut self`, which only one of the two has. Distinct
    /// from `NotMutablePointer` (nothing is dereferenced here; naming a
    /// pointer that does not appear in the source would be a lie) and from
    /// `NotMutableBinding` (no binding to declare `mut`).
    MutateTemporary,

    // -- unions --
    /// `Union { }` -- a union literal setting no field at all; unlike a
    /// struct, there's no "every field" to zero-init, and unlike an enum
    /// variant, there's no tag to pick a default from -- exactly one field
    /// must be named so the write actually has a well-defined shape.
    UnionLiteralMissingField {
        r#union: Ident,
    },
    /// `Union { a = 1; b = 2; }` -- a union literal setting more than one
    /// field; they'd overlap the same storage, so only one write is ever
    /// meaningful. `fields` lists every field name that was set, in source
    /// order.
    UnionLiteralTooManyFields {
        r#union: Ident,
        fields: Vec<Ident>,
    },

    // -- casting --
    /// `<Type>expr` where either side isn't castable at all -- scoped to
    /// numeric types and pointers (see `ResolvedType::cast_class`);
    /// structs/enums/unions/slices/`bool`/`char` have no cast support.
    InvalidCast {
        from: ResolvedType,
        to: ResolvedType,
    },
    /// `<*mut T>expr` where `expr`'s own pointer type is immutable (`*T`,
    /// not `*mut T`) -- the same directional rule `ResolvedType::accepts`
    /// already applies to pointer coercion, checked here at a cast site
    /// instead of a call/assignment site.
    CastToMutablePointer {
        from: ResolvedType,
        to: ResolvedType,
    },

    // -- overload resolution --
    /// A call (or a bare, uncalled reference) to an overloaded name where
    /// no candidate's parameters accept the arguments given -- `candidates`
    /// lists every overload's signature, so the message can show what
    /// *would* have matched.
    NoMatchingOverload {
        name: Ident,
        candidates: Vec<ResolvedFunctionType>,
    },
    /// A call (or a bare, uncalled reference) to an overloaded name where
    /// two or more candidates are equally good a match -- see
    /// `Analyzer::resolve_overload`'s scoring rule for what "equally good"
    /// means (fewest literal arguments needing a non-default type).
    /// `candidates` lists every *tied* candidate.
    AmbiguousOverload {
        name: Ident,
        candidates: Vec<ResolvedFunctionType>,
    },
    /// Two methods on the same type share a name and, once `self` is set
    /// aside, the exact same remaining parameter types -- ambiguous, since
    /// a call site has no syntax to choose between "receives self by
    /// value" and "receives self by pointer" (see
    /// `Analyzer::check_overload_duplicates`). Unlike `Redeclaration`, this
    /// is two deliberately distinct declarations, not a scope collision.
    AmbiguousSelfOverload {
        name: Ident,
        previous: Span,
    },

    // -- specs --
    /// A conform declaration requires `function` (from
    /// `spec<spec_type_args>`, possibly by way of one of its dependencies),
    /// but the type provides neither its own matching method nor does
    /// `spec` supply a default -- `implementor` is the concrete type's own
    /// name. `spec_type_args` matters now that the same spec can be
    /// implemented more than once at different type arguments (see
    /// conformance checking) -- without it, two missing
    /// requirements from two different instantiations of the same generic
    /// spec would render identically, with nothing to tell a reader which
    /// one is actually missing.
    MissingSpecFunction {
        implementor: Ident,
        spec: Ident,
        spec_type_args: Vec<ResolvedType>,
        function: Ident,
    },
    /// `for x in y { ... }` where `y`'s type doesn't *nominally* declare
    /// `: ToIterator<T>` **or** `: Iterator<T>` directly -- even if it
    /// happens to have a same-shaped `to_iterator`/`next` method (see
    /// `Analyzer::for_in_source_declares`'s doc comment for why that alone
    /// was never enough).
    ForLoopSourceNotIterable {
        r#type: ResolvedType,
    },
    /// A source has more than one `ToIterator<T>` conformance; the loop
    /// binding needs an explicit `: T` annotation to select one.
    AmbiguousForLoopElementType {
        candidates: Vec<ResolvedType>,
    },
    /// `for x : u64 in source { }` where `source` conforms to `ToIterator<T>`,
    /// but never at `u64`. Distinct from
    /// [`Self::AmbiguousForLoopElementType`]: that one means "too many to
    /// choose from", this one means "the one you named isn't there" -- which
    /// previously rendered as an ambiguity over an *empty* candidate list,
    /// naming neither the requested type nor the available ones.
    ForLoopElementTypeMismatch {
        expected: ResolvedType,
        available: Vec<ResolvedType>,
    },
    /// `base.name(...)` where `base`'s type is `spec *Spec` and `name`
    /// isn't one of `Spec`'s (flattened, dependencies included) functions.
    NoSuchSpecFunction {
        spec: Ident,
        function: Ident,
    },
    /// A spec function declared with by-value `self`/`mut self` -- rejected
    /// unconditionally, at the spec's own definition: `spec *T` dynamic
    /// dispatch erases `Self` down to a single opaque data pointer (see
    /// `Analyzer::finish_dynamic_dispatch_call`), which has no way to carry
    /// or reconstruct a full by-value copy of the concrete type. A spec
    /// function's self must always be `*self`/`*mut self`.
    SpecSelfMustBePointer {
        name: Ident,
    },
    /// A spec function declared variadic (`f(*self, ...)`) -- rejected at the
    /// spec's own definition, for the same "nothing downstream could satisfy
    /// it" reason as [`Self::SpecSelfMustBePointer`]. Omega has no variadic
    /// function *definitions*; only `extern` declarations may be variadic, so
    /// neither a `conform` block nor a spec default can supply a body with a
    /// matching signature, and every implementor would fail with a bare
    /// `MissingSpecFunction` naming a function it has no syntax to write.
    /// Lift this the day variadic definitions exist -- the `is_variadic`
    /// plumbing behind it is already complete.
    VariadicSpecFunctionUnsatisfiable {
        name: Ident,
    },
    /// An `extern` function declaration passes or returns an aggregate
    /// (struct/union/enum) *by value*. Omega's calling convention is
    /// internally consistent but is not the platform C ABI, so this shape
    /// would silently miscompile against a real C caller/callee -- rejected
    /// with the debt entry named (see `docs/14-known-issues.md`'s "Design
    /// debt worth watching") until the real C ABI lands. Scalars, pointers,
    /// slices, and everything behind a pointer stay perfectly fine.
    ExternAggregateByValue {
        r#type: ResolvedType,
    },
    /// A method call through a `spec *Spec` object where two of the spec's
    /// members (an alias of two specs declaring the same function name)
    /// could be meant -- static dispatch through a conjunction bound already
    /// rejects this shape, so dynamic dispatch must too rather than silently
    /// picking the first slot. The candidate specs are named; a narrowing
    /// cast (`<spec *A>x`) disambiguates.
    AmbiguousSpecObjectMethod {
        function: Ident,
        specs: Vec<Ident>,
    },
    /// A cast between two `spec *Spec` fat pointers that isn't a narrowing
    /// onto one of the source object's own spec sections. Only narrowing is
    /// offered: a widening cast (`<spec *AB>` from `spec *A`) has no section
    /// to invent, and a cast between unrelated specs would be a vtable
    /// reinterpretation with no offset to apply.
    SpecObjectCastImpossible {
        from: Ident,
        to: Ident,
    },
    /// `Spec::static_fn()` where nothing determines `Self` -- there is no
    /// expected type at the call site to take it from, and the bare
    /// spelling has no other place to read it. The fully-qualified form
    /// (`<Type : Spec>::fn()`) or an unambiguous `Type::fn()` names it
    /// instead.
    SpecStaticNeedsExpectedType {
        spec: Ident,
        function: Ident,
    },
    /// `Spec::static_fn()` where the declared return type is not exactly
    /// `Self`, so even an expected type cannot pin down which type
    /// implements the spec (`=> usize` never mentions it; `=> Option<Self>`
    /// would need return-type unification nothing else needs yet). The
    /// fully-qualified form names the type explicitly.
    SpecStaticReturnNotSelf {
        spec: Ident,
        function: Ident,
        return_type: String,
    },
    /// `conform Target to Alias` -- an alias names a conjunction, satisfied
    /// by conforming each member separately, never by one block conformed to
    /// the alias itself.
    ConformToAliasSpec {
        alias: Ident,
    },
    // -- annotations --
    /// `@some_unknown_name(...)` -- not a recognized annotation at all
    /// (most likely a typo). See `crate::annotations`'s applicability table.
    UnknownAnnotation {
        name: Ident,
    },
    /// A recognized annotation used on an item kind it doesn't support
    /// (e.g. `@inline` on a struct) -- `allowed` lists every item kind it
    /// *is* valid on.
    AnnotationNotApplicable {
        name: Ident,
        found: crate::annotations::ItemKind,
        allowed: Vec<crate::annotations::ItemKind>,
    },
    /// The same annotation name written twice on one item.
    DuplicateAnnotation {
        name: Ident,
    },
    /// A recognized annotation whose argument(s) don't parse into anything
    /// meaningful (wrong shape, an unrecognized mode word, a non-power-of-
    /// two `align`, ...) -- `reason` is a short, already-formatted
    /// explanation; the ways an argument can be malformed vary per
    /// annotation and don't share one structured shape worth a dedicated
    /// field each.
    InvalidAnnotationArgs {
        name: Ident,
        reason: String,
    },
    /// `@mangling(disabled)` on a function with any generic parameters --
    /// the `$$N` instantiation suffix mangling normally adds is the only
    /// thing that keeps distinct instantiations from colliding on one
    /// linker symbol once mangling is off.
    ManglingDisabledOnGeneric,
    /// `@mangling(disabled)` on a struct/enum/union method -- rejected for
    /// now: a bare method name has no owning-type prefix once mangling is
    /// off, a much easier accidental collision than a top-level function's.
    ManglingDisabledOnMethod,
    /// `@mangling(force = "...")` on a function with any generic parameters
    /// -- unlike plain `disabled`, this isn't even a *possible* collision to
    /// avoid by naming carefully: every instantiation would share the exact
    /// same hardcoded symbol, an unconditional multiple-definition error.
    /// Allowed on a method, unlike `ManglingDisabledOnMethod` -- the forced
    /// name is complete and deliberate, so there's no bare-name collision
    /// risk to guard against.
    ManglingForcedOnGeneric,
    /// A `glue` declaration targeted something other than a first-class
    /// `gap` item.
    GlueTargetNotGap {
        target: Ident,
    },
    /// A `glue` declaration omits one of its target gap's required
    /// functions.
    GlueMissingFunction {
        gap: Ident,
        function: Ident,
    },
    /// A `glue` declaration defines a function its target gap does not
    /// require.
    GlueExtraFunction {
        gap: Ident,
        function: Ident,
    },
    /// A `glue` function's parameter or return types differ from the
    /// matching gap requirement.
    GlueFunctionSignatureMismatch {
        gap: Ident,
        function: Ident,
    },
    ConformanceOrphanViolation {
        target_package: Ident,
        spec_package: Ident,
    },
    ConformTargetNotAType,
    DuplicateConformance {
        target: String,
        spec: Ident,
        previous: Span,
    },
    ConformanceExtraFunction {
        spec: Ident,
        function: Ident,
    },
    UnconstrainedConformanceParameter {
        parameter: Ident,
    },
    AmbiguousConformance {
        target: String,
        spec: Ident,
        first: Span,
    },
    ConformanceCycle {
        target: String,
        spec: Ident,
        /// The goal chain that closes the cycle, outermost first, with the
        /// re-entered goal repeated as the final link -- `(target string,
        /// spec name, span)` per link. One `note:` is rendered per
        /// consecutive pair ("proving 'S: A' requires 'S: B'").
        chain: Vec<(String, Ident, Span)>,
    },
    BlanketConformanceForeignSpec {
        spec_package: Ident,
    },
    PrimitiveOutsideCore,
    PrimitiveTargetNotAllowed {
        target: String,
    },
    DuplicatePrimitiveTarget {
        target: String,
        previous: Span,
    },
    AmbiguousConformanceStatic {
        target: String,
        function: Ident,
        specs: Vec<Ident>,
    },
    MethodNotInScope {
        method: Ident,
        spec: Ident,
        /// The receiver's own type -- the concrete half of the
        /// fully-qualified spelling the renderer suggests
        /// (`<Type : Spec>::method(recv, ...)`).
        r#type: ResolvedType,
    },
    /// Two or more different `glue` declarations implement the same gap --
    /// exactly one glue is allowed per gap, project-wide. Anchored at the
    /// gap's own declaration (a whole-program check, run once at the end of
    /// compilation -- see `Driver::sweep_gaps` -- rather than at either
    /// individual `glue` site, since neither is more "at fault" than the
    /// other). `glues` names every conflicting implementor found, in
    /// discovery order.
    MultipleGluesForGap {
        gap: Ident,
        glues: Vec<Ident>,
    },
    /// `comp <expr>` couldn't be evaluated at compile time -- `reason` names
    /// the specific construct that actually blocked it (an already-
    /// formatted description of a `comp_eval::CompErrorKind`, from
    /// `Analyzer::analyze_comp`), and `trace` is the call-site chain from
    /// the outermost `comp` down to wherever `reason` happened, outermost
    /// first -- empty when the failure was directly inside the outermost
    /// evaluation, with no intervening call. See `docs/19-compile-time-evaluation.md`.
    CompEvalFailed {
        reason: String,
        trace: Vec<Span>,
    },
    /// `mut comp a := ...;` -- a `comp` binding carries no storage of its
    /// own (every reference to it is substituted with its already-known
    /// value at compile time), so a later mutation could never be observed
    /// by anything that already substituted it -- incoherent, not just
    /// discouraged.
    MutCompBinding,
    /// `ident := value;` at item level (no `comp` on the binding) whose
    /// `value` doesn't resolve to a compile-time-known `CheckedExpr::Const`
    /// -- a non-`comp` top-level binding gets real storage (unlike a
    /// `comp` binding), but its initial value still has to be known before
    /// codegen runs: there's no runtime constructor/init-order machinery
    /// (a genuinely runtime-computed top-level global is a distinct,
    /// larger feature nobody has built). The fix is an explicit `comp
    /// <expr>` initializer, not `comp` on the binding -- see
    /// `Analyzer::analyze_global_walrus`'s own doc comment, and
    /// `docs/19-compile-time-evaluation.md`.
    TopLevelValueNotComp,
    /// A `struct`/`union` whose fields (if any) all resolve to zero-sized
    /// types -- unlike `marker`, a `struct`/`union` is meant to hold real
    /// data, so this is rejected outright rather than silently accepted as
    /// a `marker` would be. Checked against the type's own full,
    /// recursively-flattened leaf list (`layout::is_zero_sized`), so this
    /// also catches a struct whose only field is itself another zero-sized
    /// type, and a generic struct/union whose fields happen to all resolve
    /// to a zero-sized type for one particular instantiation -- not just a
    /// literally empty field list.
    ZeroSizedAggregate {
        name: Ident,
        is_union: bool,
    },
}

impl fmt::Display for AnalysisErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedType(e) => write!(f, "{e}"),
            Self::UndefinedVariable { name, .. } => {
                write!(f, "cannot find '{}' in this scope", name.as_ref())
            }
            Self::UndefinedPathHead { name, .. } => {
                write!(f, "cannot find '{}' in this scope", name.as_ref())
            }
            Self::NotAStruct { found } => {
                write!(f, "field access on '{found}', which is not a struct")
            }
            Self::NoSuchField { field, base } => {
                write!(f, "no field '{}' on '{base}'", field.as_ref())
            }
            Self::FieldNotVisible { field, base } => {
                write!(f, "'{}' on '{base}' is not visible here", field.as_ref())
            }
            Self::MethodNotVisible { method, base } => {
                write!(f, "'{}' on '{base}' is not visible here", method.as_ref())
            }
            Self::NotAnArray { found } => write!(f, "cannot index a value of type '{found}'"),
            Self::WrongArgumentCount { expected, found } => {
                write!(
                    f,
                    "this function takes {expected} {} but {found} {} supplied",
                    plural(*expected, "argument"),
                    if *found == 1 { "was" } else { "were" }
                )
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
                write!(
                    f,
                    "'{}' is declared multiple times in this scope",
                    name.as_ref()
                )
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
            Self::NotAPointer { found } => {
                write!(f, "cannot dereference a value of type '{found}'")
            }
            Self::AddressOfNotAPlace => {
                write!(f, "cannot take the address of this expression")
            }
            Self::InvalidBinaryOperand { op, r#type } => write!(
                f,
                "cannot apply '{}' to a value of type '{}'",
                op.symbol(),
                r#type
            ),
            Self::CharArithmeticNotAllowed { op } => {
                write!(f, "cannot apply '{op}' to a value of type 'char'")
            }
            Self::PointerPairArithmetic { op } => write!(
                f,
                "cannot apply '{}' to two pointer values",
                op.symbol()
            ),
            Self::InvalidNegateOperand { r#type } => {
                write!(f, "cannot negate a value of type '{}'", r#type)
            }
            Self::InvalidBitNotOperand { r#type } => {
                write!(f, "cannot apply '~' to a value of type '{}'", r#type)
            }
            Self::FloatBitwiseOperand => {
                write!(
                    f,
                    "bitwise/shift operators are not supported on floating-point operands"
                )
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
                "mismatched types: slice bound must be 'i32', found '{}'",
                r#type
            ),
            Self::MissingSliceEnd => {
                write!(f, "there is no length here to infer a range end from")
            }
            Self::CompPointerSliceNotSupported => {
                write!(f, "slicing a 'comp'-bound unsized array is not supported")
            }
            Self::RangeNotAllowedHere => {
                write!(
                    f,
                    "bare '..' has no context here"
                )
            }
            Self::RangeNeedsBounded { r#type } => write!(f, "open range needs Bounded for '{type}'"),
            Self::EmptyArrayLiteral => {
                write!(f, "cannot infer the element type of an empty array literal")
            }
            Self::ArrayElementTypeMismatch { .. } => {
                write!(f, "mismatched types in array literal")
            }
            Self::ArraySizeNotInferable => {
                write!(f, "cannot infer this array's length")
            }
            Self::ConstSliceCannotBeMutable => {
                write!(f, "a compile-time slice cannot be mutable")
            }
            Self::ConstSliceElementNotConstant => {
                write!(f, "compile-time slice elements must be literal constants")
            }
            Self::ConstSliceElementTypeMismatch { expected, found } => {
                write!(f, "mismatched types: expected '{expected}', found {found}")
            }
            Self::BinaryOperandTypeMismatch { left, right, .. } => {
                write!(f, "mismatched types: '{left}' and '{right}'")
            }
            Self::FloatRemainder => {
                write!(f, "'%' is not supported on floating-point operands")
            }
            Self::NonBoolCondition { r#type } => write!(
                f,
                "mismatched types: condition must be 'bool', found '{}'",
                r#type
            ),
            Self::IfBranchTypeMismatch { .. } => {
                write!(f, "'if' and 'else' branches have incompatible types")
            }
            Self::ReturnTypeMismatch { expected, found } => write!(
                f,
                "mismatched types: expected return type '{expected}', found '{found}'"
            ),
            Self::IncrementTargetNotAPlace => {
                write!(f, "invalid '++'/'--' operand")
            }
            Self::InvalidIncrementOperand { r#type } => {
                write!(f, "cannot increment/decrement a value of type '{}'", r#type)
            }
            Self::ForLoopMissingCondition => {
                write!(f, "this 'for' loop is missing its condition clause")
            }
            Self::BreakOutsideLoop => write!(f, "'break' outside of a loop"),
            Self::ContinueOutsideLoop => write!(f, "'continue' outside of a loop"),
            Self::ModuleResolution(e) => write!(f, "{e}"),
            Self::MacroDependencyTooPrivate { item, macro_visibility, item_visibility } => write!(
                f,
                "macro-visible item '{}' is {} but its macro is {}",
                item.as_ref(), item_visibility, macro_visibility
            ),
            Self::NotAValue(path) => write!(f, "'{}' is a type, not a value", join(path)),
            Self::UnresolvedGenericParam(ident) => write!(
                f,
                "cannot infer type parameter '{}' from this call's arguments",
                ident.as_ref()
            ),
            Self::GenericParamFromFatPointer { parameter, .. } => write!(
                f,
                "cannot infer type parameter '{}' from this call's arguments",
                parameter.as_ref()
            ),
            Self::UnresolvedLiteralGeneric { r#type, generics } => write!(
                f,
                "cannot infer type argument(s) {} of '{}' here",
                generics
                    .iter()
                    .map(|g| format!("'{}'", g.as_ref()))
                    .collect::<Vec<_>>()
                    .join(", "),
                r#type.as_ref()
            ),
            Self::DeferInsideLoopNotSupported => {
                write!(f, "'defer' is not supported inside a loop body")
            }
            Self::ReturnInsideDefer => write!(f, "'return' is not supported inside a 'defer' body"),
            Self::NestedDeferNotSupported => {
                write!(f, "'defer' is not supported inside another 'defer' body")
            }
            Self::StructLiteralNotAStruct { found } => {
                write!(
                    f,
                    "cannot build a value of type '{found}' with a struct literal"
                )
            }
            Self::DuplicateFieldInitializer { field, .. } => {
                write!(f, "field '{}' is set more than once", field.as_ref())
            }
            Self::FieldTypeMismatch {
                field,
                expected,
                found,
            } => write!(
                f,
                "mismatched types: field '{}' is '{expected}', found '{found}'",
                field.as_ref()
            ),
            Self::MissingFieldInitializers { r#struct, missing } => {
                write!(
                    f,
                    "missing {} in initializer of '{}'",
                    field_list(missing),
                    r#struct.as_ref()
                )
            }
            Self::NoSuchStructFunction {
                r#struct, function, ..
            } => {
                write!(
                    f,
                    "no function '{}' on '{}'",
                    function.as_ref(),
                    r#struct.as_ref()
                )
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
                write!(
                    f,
                    "enum tags must be integers, but this tag is declared as '{found}'"
                )
            }
            Self::EnumHeaderFieldUnsupportedType { field, .. } => {
                write!(
                    f,
                    "header field '{}' has a type that cannot hold a constant",
                    field.as_ref()
                )
            }
            Self::EnumVariantArgCount {
                variant,
                expected,
                found,
                ..
            } => {
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
            Self::DuplicateEnumTag {
                variant,
                value,
                previous_variant,
                ..
            } => {
                write!(
                    f,
                    "variants '{}' and '{}' share the tag value {value}",
                    previous_variant.as_ref(),
                    variant.as_ref()
                )
            }
            Self::EnumFieldNameCollision { field, variant } => match variant {
                Some(variant) => write!(
                    f,
                    "field '{}' of variant '{}' collides with another field of this enum",
                    field.as_ref(),
                    variant.as_ref()
                ),
                None => write!(
                    f,
                    "'{}' is declared more than once in this enum",
                    field.as_ref()
                ),
            },
            Self::EnumLiteralWithoutVariant { r#enum, .. } => {
                write!(
                    f,
                    "cannot build enum '{}' without naming a variant",
                    r#enum.as_ref()
                )
            }
            Self::NoSuchEnumMember { r#enum, name, .. } => {
                write!(
                    f,
                    "no variant or function '{}' on enum '{}'",
                    name.as_ref(),
                    r#enum.as_ref()
                )
            }
            Self::EnumVariantMissingBody {
                r#enum, variant, ..
            } => {
                write!(
                    f,
                    "variant '{}::{}' has fields that must be initialized",
                    r#enum.as_ref(),
                    variant.as_ref()
                )
            }
            Self::EnumVariantHasNoBody { r#enum, variant } => {
                write!(
                    f,
                    "variant '{}::{}' has no fields to initialize",
                    r#enum.as_ref(),
                    variant.as_ref()
                )
            }
            Self::StructLiteralPathTooDeep { r#struct, name } => {
                write!(
                    f,
                    "'{}' is a struct, so '{}' cannot be one of its variants",
                    r#struct.as_ref(),
                    name.as_ref()
                )
            }
            Self::EnumFieldWrongVariant { field, owner, .. } => {
                write!(
                    f,
                    "field '{}' belongs to a different variant ('{}')",
                    field.as_ref(),
                    owner.as_ref()
                )
            }
            Self::EnumFieldVariantUnknown { field, r#enum, .. } => {
                write!(
                    f,
                    "cannot access variant field '{}' on a value whose '{}' variant is unknown",
                    field.as_ref(),
                    r#enum.as_ref()
                )
            }
            Self::NoSuchEnumField { field, r#enum, .. } => {
                write!(
                    f,
                    "no field '{}' on enum '{}'",
                    field.as_ref(),
                    r#enum.as_ref()
                )
            }
            Self::GenericPathTooDeep { r#type } => {
                write!(f, "path continues too far past '{}'", r#type.as_ref())
            }
            Self::EnumFieldImmutable { field } => {
                write!(f, "cannot assign to '{}' of an enum value", field.as_ref())
            }
            Self::EnumHeaderFieldInLiteral { field } => {
                write!(
                    f,
                    "header field '{}' cannot be initialized at a construction site",
                    field.as_ref()
                )
            }
            Self::StructPathTooDeep { r#struct, function } => {
                write!(
                    f,
                    "'{}::{}' is a function; there is nothing to look up inside it",
                    r#struct.as_ref(),
                    function.as_ref()
                )
            }
            Self::NotAModule { name } => {
                write!(f, "'{}' is a value, not a module or type", name.as_ref())
            }
            Self::PatternValueNotConstant => write!(f, "match patterns must be literal constants"),
            Self::NoSuchVariantInPattern { r#enum, name, .. } => {
                write!(
                    f,
                    "no variant '{}' on enum '{}'",
                    name.as_ref(),
                    r#enum.as_ref()
                )
            }
            Self::PatternNotEnumVariant { r#enum } => {
                write!(
                    f,
                    "'{}' can only be matched by naming a variant",
                    r#enum.as_ref()
                )
            }
            Self::PatternIsEnumVariant {
                r#enum,
                variant,
                scrutinee,
            } => write!(
                f,
                "mismatched types: expected '{scrutinee}', found '{}::{}'",
                r#enum.as_ref(),
                variant.as_ref()
            ),
            Self::PatternTypeMismatch { expected, found } => {
                write!(
                    f,
                    "mismatched types: expected '{expected}', found '{found}'"
                )
            }
            Self::UnsupportedMatchScrutinee { r#type } => {
                write!(f, "cannot match on a value of type '{type}'")
            }
            Self::OverlappingMatchArm { .. } => write!(f, "overlapping match arms"),
            Self::NonExhaustiveMatchEnum { r#enum, .. } => {
                write!(
                    f,
                    "match on '{}' does not cover every variant",
                    r#enum.as_ref()
                )
            }
            Self::NonExhaustiveMatchValue { r#type, .. } => {
                write!(f, "match on '{type}' does not cover every value")
            }
            Self::CatchAllRangeNotInferable { gaps } => {
                write!(
                    f,
                    "'..' can't be inferred: {gaps} disjoint ranges are left uncovered, not one"
                )
            }
            Self::CatchAllPatternRedundant => write!(f, "'..' has nothing left to cover"),
            Self::MultipleCatchAllPatterns { .. } => write!(f, "more than one '..' catch-all arm"),
            Self::MatchArmTypeMismatch { .. } => write!(f, "'match' arms have incompatible types"),
            Self::NotMutableBinding { ident } => {
                write!(f, "cannot mutate '{}': not declared 'mut'", ident.as_ref())
            }
            Self::NotMutablePointer => write!(f, "cannot mutate through an immutable pointer"),
            Self::MutateTemporary => write!(f, "cannot mutate a temporary value"),
            Self::UnionLiteralMissingField { r#union } => {
                write!(f, "union literal for '{}' sets no field", r#union.as_ref())
            }
            Self::UnionLiteralTooManyFields { r#union, .. } => {
                write!(
                    f,
                    "union literal for '{}' sets more than one field",
                    r#union.as_ref()
                )
            }
            Self::InvalidCast { from, to } => write!(f, "cannot cast '{from}' to '{to}'"),
            Self::CastToMutablePointer { from, to } => {
                write!(
                    f,
                    "cannot cast '{from}' to '{to}': target is mutable, source is not"
                )
            }
            Self::NoMatchingOverload { name, .. } => {
                write!(f, "no overload of '{}' matches this call", name.as_ref())
            }
            Self::AmbiguousOverload { name, .. } => {
                write!(f, "ambiguous reference to overloaded '{}'", name.as_ref())
            }
            Self::AmbiguousSelfOverload { name, .. } => {
                write!(
                    f,
                    "'{}' is declared twice, differing only in how it receives 'self'",
                    name.as_ref()
                )
            }
            Self::MissingSpecFunction {
                implementor,
                spec,
                spec_type_args,
                function,
            } => write!(
                f,
                "'{}' does not implement spec '{}': missing '{}'",
                implementor.as_ref(),
                generic_name(spec, spec_type_args),
                function.as_ref()
            ),
            Self::ForLoopSourceNotIterable { r#type } => {
                write!(
                    f,
                    "'{type}' does not implement 'ToIterator<T>' or 'Iterator<T>'"
                )
            }
            Self::AmbiguousForLoopElementType { candidates } => write!(
                f,
                "for-loop source has ambiguous element type: {}",
                candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            ),
            Self::ForLoopElementTypeMismatch { expected, available } => write!(
                f,
                "for-loop source produces no '{expected}' elements (it produces: {})",
                available.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            ),
            Self::NoSuchSpecFunction { spec, function } => {
                write!(
                    f,
                    "no function '{}' on spec '{}'",
                    function.as_ref(),
                    spec.as_ref()
                )
            }
            Self::SpecSelfMustBePointer { name } => {
                write!(
                    f,
                    "spec function '{}' must receive 'self' by pointer",
                    name.as_ref()
                )
            }
            Self::ExternAggregateByValue { r#type } => {
                write!(f, "'{type}' cannot cross an `extern` boundary by value")
            }
            Self::VariadicSpecFunctionUnsatisfiable { name } => {
                write!(
                    f,
                    "spec function '{}' is variadic, which no implementor could satisfy",
                    name.as_ref()
                )
            }
            Self::AmbiguousSpecObjectMethod { function, specs } => {
                let specs: Vec<&str> = specs.iter().map(|s| s.as_ref()).collect();
                write!(
                    f,
                    "ambiguous method '{}' through a spec object -- declared by {}; \
                     narrow the object with a cast (`<spec *{}>x`) to pick one",
                    function.as_ref(),
                    specs.join(", "),
                    specs.first().map_or("<spec>", |s| *s)
                )
            }
            Self::SpecObjectCastImpossible { from, to } => {
                write!(
                    f,
                    "cannot cast `spec *{from}` to `spec *{to}` -- only narrowing onto one of \
                     the object's own specs is possible; widenings and cross-spec casts have no \
                     vtable section to point at",
                    from = from.as_ref(),
                    to = to.as_ref(),
                )
            }
            Self::SpecStaticNeedsExpectedType { spec, function } => {
                write!(
                    f,
                    "cannot determine which type implements '{spec}' for '{function}' -- there \
                     is no expected type here to take 'Self' from",
                    spec = spec.as_ref(),
                    function = function.as_ref(),
                )
            }
            Self::SpecStaticReturnNotSelf { spec, function, return_type } => {
                write!(
                    f,
                    "cannot determine which type implements '{spec}' for '{function}' -- its \
                     return type '{return_type}' does not say which type implements it",
                    spec = spec.as_ref(),
                    function = function.as_ref(),
                )
            }
            Self::ConformToAliasSpec { alias } => {
                write!(
                    f,
                    "cannot conform to spec alias '{}' -- an alias names a combination of specs \
                     and is not itself implementable; conform to each member separately",
                    alias.as_ref()
                )
            }
            Self::UnknownAnnotation { name } => {
                write!(f, "unknown annotation '@{}'", name.as_ref())
            }
            Self::AnnotationNotApplicable { name, found, .. } => {
                write!(f, "'@{}' cannot be applied to {found}", name.as_ref())
            }
            Self::DuplicateAnnotation { name } => {
                write!(f, "duplicate '@{}' annotation", name.as_ref())
            }
            Self::InvalidAnnotationArgs { name, reason } => {
                write!(f, "invalid arguments for '@{}': {reason}", name.as_ref())
            }
            Self::ManglingDisabledOnGeneric => {
                write!(f, "cannot disable mangling on a generic function")
            }
            Self::ManglingDisabledOnMethod => write!(f, "cannot disable mangling on a method"),
            Self::ManglingForcedOnGeneric => write!(
                f,
                "cannot force a mangled symbol name on a generic function"
            ),
            Self::GlueTargetNotGap { target } => write!(f, "'{}' is not a gap", target.as_ref()),
            Self::GlueMissingFunction { gap, function } => {
                write!(
                    f,
                    "glue for gap '{}' is missing function '{}'",
                    gap.as_ref(),
                    function.as_ref()
                )
            }
            Self::GlueExtraFunction { gap, function } => {
                write!(
                    f,
                    "glue for gap '{}' has no function '{}'",
                    gap.as_ref(),
                    function.as_ref()
                )
            }
            Self::GlueFunctionSignatureMismatch { gap, function } => {
                write!(
                    f,
                    "glue function '{}' does not match gap '{}'",
                    function.as_ref(),
                    gap.as_ref()
                )
            }
            Self::ConformanceOrphanViolation {
                target_package,
                spec_package,
            } => write!(
                f,
                "cannot conform a type from '{}' to a spec from '{}'",
                target_package.as_ref(),
                spec_package.as_ref()
            ),
            Self::ConformTargetNotAType => write!(f, "conform target is not a concrete type"),
            Self::DuplicateConformance { target, spec, .. } => {
                write!(f, "duplicate conform for '{target}: {}'", spec.as_ref())
            }
            Self::ConformanceExtraFunction { spec, function } => {
                write!(
                    f,
                    "conform declares '{}' which is not in spec '{}'",
                    function.as_ref(),
                    spec.as_ref()
                )
            }
            Self::UnconstrainedConformanceParameter { parameter } => {
                write!(
                    f,
                    "conformance parameter '{}' is not fixed by the target",
                    parameter.as_ref()
                )
            }
            Self::AmbiguousConformance { target, spec, .. } => {
                write!(f, "ambiguous conform for '{target}: {}'", spec.as_ref())
            }
            Self::ConformanceCycle { target, spec, .. } => {
                write!(f, "cyclic conformance while proving '{target}: {}'", spec.as_ref())
            }
            Self::BlanketConformanceForeignSpec { spec_package } => {
                write!(f, "a blanket conform cannot implement a foreign spec from '{}'", spec_package.as_ref())
            }
            Self::PrimitiveOutsideCore => {
                write!(f, "primitive blocks may only be declared in core")
            }
            Self::PrimitiveTargetNotAllowed { target } => {
                write!(f, "'{target}' is not a primitive target")
            }
            Self::DuplicatePrimitiveTarget { target, .. } => {
                write!(f, "duplicate primitive block for '{target}'")
            }
            Self::AmbiguousConformanceStatic {
                target, function, ..
            } => {
                write!(
                    f,
                    "conforming static function '{}::{}' is ambiguous",
                    target,
                    function.as_ref()
                )
            }
            Self::MethodNotInScope { method, spec, .. } => {
                write!(
                    f,
                    "method '{}' comes from spec '{}' but is not in this bound context",
                    method.as_ref(),
                    spec.as_ref()
                )
            }
            Self::MultipleGluesForGap { gap, glues } => write!(
                f,
                "more than one glue declaration implements gap '{}' ({})",
                gap.as_ref(),
                glues
                    .iter()
                    .map(Ident::as_ref)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::CompEvalFailed { reason, .. } => write!(
                f,
                "cannot evaluate this expression at compile time: {reason}"
            ),
            Self::MutCompBinding => write!(f, "a 'comp' binding cannot be 'mut'"),
            Self::TopLevelValueNotComp => {
                write!(f, "a top-level binding's value must be compile-time-known")
            }
            Self::ZeroSizedAggregate { name, is_union } => {
                let kind = if *is_union { "union" } else { "struct" };
                write!(f, "{kind} '{}' has no sized fields", name.as_ref())
            }
        }
    }
}
