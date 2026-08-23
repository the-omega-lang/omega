use super::*;

#[derive(Debug, Clone)]
pub enum AnalysisErrorKind {
    UnresolvedType(TypeResolutionError),
    UndefinedVariable {
        name: Ident,
        similar: Option<Ident>,
    },
    UndefinedPathHead {
        name: Ident,
        similar_module: Option<Ident>,
        similar_type: Option<Ident>,
    },
    NotAStruct {
        found: ResolvedType,
    },
    NoSuchField {
        field: Ident,
        base: ResolvedType,
    },
    FieldNotVisible {
        field: Ident,
        base: ResolvedType,
    },
    MethodNotVisible {
        method: Ident,
        base: ResolvedType,
    },
    NotAnArray {
        found: ResolvedType,
    },
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
    Redeclaration {
        name: Ident,
        previous: Option<Span>,
    },
    AssignmentTargetNotAPlace,
    AssignmentTypeMismatch {
        target: ResolvedType,
        value: ResolvedType,
    },
    CompoundAssignTargetNotAPlace,
    NumberLiteralOutOfRange {
        literal: String,
        r#type: ResolvedType,
    },
    NotAPointer {
        found: ResolvedType,
    },
    AddressOfNotAPlace,
    InvalidBinaryOperand {
        op: BinaryOp,
        r#type: ResolvedType,
    },
    CharArithmeticNotAllowed {
        op: String,
    },
    PointerPairArithmetic {
        op: BinaryOp,
    },
    InvalidNegateOperand {
        r#type: ResolvedType,
    },
    InvalidBitNotOperand {
        r#type: ResolvedType,
    },
    InvalidNotOperand {
        r#type: ResolvedType,
    },
    InvalidLogicalOperand {
        op: &'static str,
        r#type: ResolvedType,
    },
    FloatBitwiseOperand,
    NotSliceable {
        found: ResolvedType,
    },
    SliceRequiresAddressOf,
    ImmutableSliceSource,
    InvalidSliceBound {
        r#type: ResolvedType,
    },
    MissingSliceEnd,
    CompPointerSliceNotSupported,
    RangeNotAllowedHere,
    RangeNeedsBounded {
        r#type: ResolvedType,
    },
    EmptyArrayLiteral,
    ArrayElementTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    ArraySizeNotInferable,
    ConstSliceCannotBeMutable,
    ConstSliceElementNotConstant,
    ConstSliceElementTypeMismatch {
        expected: ResolvedType,
        found: String,
    },
    BinaryOperandTypeMismatch {
        left: ResolvedType,
        left_span: Span,
        right: ResolvedType,
        right_span: Span,
    },
    FloatRemainder,
    NonBoolCondition {
        r#type: ResolvedType,
    },
    IfBranchTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    ReturnTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    InvalidMainSignature,
    IncrementTargetNotAPlace,
    InvalidIncrementOperand {
        r#type: ResolvedType,
    },
    ForLoopMissingCondition,
    BreakOutsideLoop,
    ContinueOutsideLoop,
    ModuleResolution(crate::resolver::ResolveError),
    MacroDependencyTooPrivate {
        item: Ident,
        macro_visibility: omega_parser::prelude::Visibility,
        item_visibility: omega_parser::prelude::Visibility,
    },
    NotAValue(Vec<Ident>),
    UnresolvedGenericParam(Ident),
    GenericParamFromFatPointer {
        parameter: Ident,
        found: ResolvedType,
    },
    UnresolvedLiteralGeneric {
        r#type: Ident,
        generics: Vec<Ident>,
    },
    DeferInsideLoopNotSupported,
    ReturnInsideDefer,
    NestedDeferNotSupported,
    StructLiteralNotAStruct {
        found: ResolvedType,
    },
    DuplicateFieldInitializer {
        field: Ident,
        previous: Span,
    },
    FieldTypeMismatch {
        field: Ident,
        expected: ResolvedType,
        found: ResolvedType,
    },
    MissingFieldInitializers {
        r#struct: Ident,
        missing: Vec<Ident>,
    },
    NoSuchStructFunction {
        r#struct: Ident,
        function: Ident,
        similar: Option<Ident>,
    },
    MemberFunctionWithoutInstance {
        r#struct: Ident,
        function: Ident,
    },
    StaticFunctionOnInstance {
        r#struct: Ident,
        function: Ident,
    },
    StaticAccessOnNonStruct {
        found: ResolvedType,
    },
    StructPathTooDeep {
        r#struct: Ident,
        function: Ident,
    },
    NotAModule {
        name: Ident,
    },
    EnumTagNotFirst,
    EnumTagNotInteger {
        found: ResolvedType,
    },
    EnumImplicitTagOutOfRange {
        variant: Ident,
        value: usize,
        r#type: ResolvedType,
    },
    EnumHeaderFieldUnsupportedType {
        field: Ident,
        found: ResolvedType,
    },
    EnumVariantArgCount {
        variant: Ident,
        expected: usize,
        found: usize,
        has_tag: bool,
    },
    EnumValueNotConstant,
    EnumValueTypeMismatch {
        expected: ResolvedType,
        found: String,
    },
    DuplicateEnumTag {
        variant: Ident,
        value: String,
        previous_variant: Ident,
        previous: Span,
    },
    EnumFieldNameCollision {
        field: Ident,
        variant: Option<Ident>,
    },
    EnumLiteralWithoutVariant {
        r#enum: Ident,
        example: Ident,
    },
    NoSuchEnumMember {
        r#enum: Ident,
        name: Ident,
        similar_variant: Option<Ident>,
        similar_function: Option<Ident>,
    },
    EnumVariantMissingBody {
        r#enum: Ident,
        variant: Ident,
        fields: Vec<Ident>,
    },
    EnumVariantHasNoBody {
        r#enum: Ident,
        variant: Ident,
    },
    StructLiteralPathTooDeep {
        r#struct: Ident,
        name: Ident,
    },
    EnumFieldWrongVariant {
        field: Ident,
        owner: Ident,
        actual: Ident,
    },
    EnumFieldVariantUnknown {
        field: Ident,
        r#enum: Ident,
        owner: Ident,
    },
    NoSuchEnumField {
        field: Ident,
        r#enum: Ident,
        similar: Option<Ident>,
    },
    GenericPathTooDeep {
        r#type: Ident,
    },
    EnumFieldImmutable {
        field: Ident,
    },
    EnumHeaderFieldInLiteral {
        field: Ident,
    },

    PatternValueNotConstant,
    NoSuchVariantInPattern {
        r#enum: Ident,
        name: Ident,
        similar: Option<Ident>,
    },
    PatternNotEnumVariant {
        r#enum: Ident,
    },
    PatternIsEnumVariant {
        r#enum: Ident,
        variant: Ident,
        scrutinee: ResolvedType,
    },
    PatternTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },
    UnsupportedMatchScrutinee {
        r#type: ResolvedType,
    },
    OverlappingMatchArm {
        previous: Span,
    },
    NonExhaustiveMatchEnum {
        r#enum: Ident,
        missing: Vec<Ident>,
    },
    NonExhaustiveMatchAnonymousEnum {
        r#enum: ResolvedType,
        missing: Vec<ResolvedType>,
    },
    AnonymousEnumPatternNotAType {
        r#enum: ResolvedType,
    },
    NotAnAnonymousEnumMember {
        found: ResolvedType,
        r#enum: ResolvedType,
    },
    AnonymousEnumNotRefined {
        r#enum: ResolvedType,
    },
    AnonymousEnumConformTarget {
        r#enum: ResolvedType,
    },
    NonExhaustiveMatchValue {
        r#type: ResolvedType,
        gaps: Vec<String>,
    },
    CatchAllRangeNotInferable {
        gaps: usize,
    },
    CatchAllPatternRedundant,
    MultipleCatchAllPatterns {
        previous: Span,
    },
    MatchArmTypeMismatch {
        expected: ResolvedType,
        found: ResolvedType,
    },

    NotMutableBinding {
        ident: Ident,
    },
    NotMutablePointer,
    MutateTemporary,

    UnionLiteralMissingField {
        r#union: Ident,
    },
    UnionLiteralTooManyFields {
        r#union: Ident,
        fields: Vec<Ident>,
    },

    InvalidCast {
        from: ResolvedType,
        to: ResolvedType,
    },
    CastToMutablePointer {
        from: ResolvedType,
        to: ResolvedType,
    },

    NoMatchingOverload {
        name: Ident,
        candidates: Vec<ResolvedFunctionType>,
    },
    AmbiguousOverload {
        name: Ident,
        candidates: Vec<ResolvedFunctionType>,
    },
    AmbiguousSelfOverload {
        name: Ident,
        previous: Span,
    },

    MissingSpecFunction {
        implementor: Ident,
        spec: Ident,
        spec_type_args: Vec<ResolvedType>,
        function: Ident,
    },
    ForLoopSourceNotIterable {
        r#type: ResolvedType,
    },
    AmbiguousForLoopElementType {
        candidates: Vec<ResolvedType>,
    },
    ForLoopElementTypeMismatch {
        expected: ResolvedType,
        available: Vec<ResolvedType>,
    },
    NoSuchSpecFunction {
        spec: Ident,
        function: Ident,
    },
    SpecSelfMustBePointer {
        name: Ident,
    },
    VariadicSpecFunctionUnsatisfiable {
        name: Ident,
    },
    ForeignAggregateByValue {
        r#type: ResolvedType,
    },
    GenericForeignFunctionUnsupported,
    AmbiguousSpecObjectMethod {
        function: Ident,
        specs: Vec<Ident>,
    },
    SpecObjectCastImpossible {
        from: Ident,
        to: Ident,
    },
    SpecStaticNeedsExpectedType {
        spec: Ident,
        function: Ident,
    },
    SpecStaticReturnNotSelf {
        spec: Ident,
        function: Ident,
        return_type: String,
    },
    UnknownAnnotation {
        name: Ident,
    },
    AnnotationNotApplicable {
        name: Ident,
        found: crate::annotations::ItemKind,
        allowed: Vec<crate::annotations::ItemKind>,
    },
    DuplicateAnnotation {
        name: Ident,
    },
    InvalidAnnotationArgs {
        name: Ident,
        reason: String,
    },
    ManglingDisabledOnGeneric,
    ManglingDisabledOnMethod,
    ManglingForcedOnGeneric,
    GlueTargetNotGap {
        target: Ident,
    },
    GlueMissingFunction {
        gap: Ident,
        function: Ident,
    },
    GlueExtraFunction {
        gap: Ident,
        function: Ident,
    },
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
        r#type: ResolvedType,
    },
    MultipleGluesForGap {
        gap: Ident,
        glues: Vec<Ident>,
    },
    CompEvalFailed {
        reason: String,
        trace: Vec<Span>,
    },
    MutCompBinding,
    TopLevelValueNotComp,
    ZeroSizedAggregate {
        name: Ident,
        is_union: bool,
    },
    AsmRegNotOneRegisterOperand {
        r#type: ResolvedType,
    },
    AsmConstNotComp,
    AsmConstUnsupportedShape,
    AsmUnknownBinding {
        text: String,
    },
    AsmAmbiguousBinding {
        text: String,
    },
    NakedInlineConflict,
    InvalidNakedBody,
    AsmRegInNakedFunction,
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
            Self::PointerPairArithmetic { op } => {
                write!(f, "cannot apply '{}' to two pointer values", op.symbol())
            }
            Self::InvalidNegateOperand { r#type } => {
                write!(f, "cannot negate a value of type '{}'", r#type)
            }
            Self::InvalidBitNotOperand { r#type } => {
                write!(f, "cannot apply '~' to a value of type '{}'", r#type)
            }
            Self::InvalidNotOperand { r#type } => {
                write!(f, "cannot apply '!' to a value of type '{}'", r#type)
            }
            Self::InvalidLogicalOperand { op, r#type } => {
                write!(f, "'{}' requires 'bool' operands, found '{}'", op, r#type)
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
                write!(f, "bare '..' has no context here")
            }
            Self::RangeNeedsBounded { r#type } => {
                write!(f, "open range needs Bounded for '{type}'")
            }
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
            Self::InvalidMainSignature => write!(f, "invalid 'main' signature"),
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
            Self::MacroDependencyTooPrivate {
                item,
                macro_visibility,
                item_visibility,
            } => write!(
                f,
                "macro-visible item '{}' is {} but its macro is {}",
                item.as_ref(),
                item_visibility,
                macro_visibility
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
            Self::EnumImplicitTagOutOfRange {
                variant,
                value,
                r#type,
            } => write!(
                f,
                "implicit tag {value} for variant '{}' does not fit in '{type}'",
                variant.as_ref(),
            ),
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
            Self::NonExhaustiveMatchAnonymousEnum { r#enum, .. } => {
                write!(f, "match on '{enum}' does not cover every member")
            }
            Self::AnonymousEnumPatternNotAType { r#enum } => {
                write!(
                    f,
                    "a match arm on '{enum}' must name one of its member types"
                )
            }
            Self::NotAnAnonymousEnumMember { found, r#enum } => {
                write!(f, "'{found}' is not a member of '{enum}'")
            }
            Self::AnonymousEnumNotRefined { r#enum } => {
                write!(f, "'{enum}' has no members of its own")
            }
            Self::AnonymousEnumConformTarget { r#enum } => {
                write!(f, "'{enum}' has no declaration to conform")
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
                candidates
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::ForLoopElementTypeMismatch {
                expected,
                available,
            } => write!(
                f,
                "for-loop source produces no '{expected}' elements (it produces: {})",
                available
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
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
            Self::ForeignAggregateByValue { r#type } => {
                write!(f, "'{type}' cannot cross a 'foreign' boundary by value")
            }
            Self::GenericForeignFunctionUnsupported => {
                write!(f, "a generic 'foreign' function is not yet supported")
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
            Self::SpecStaticReturnNotSelf {
                spec,
                function,
                return_type,
            } => {
                write!(
                    f,
                    "cannot determine which type implements '{spec}' for '{function}' -- its \
                     return type '{return_type}' does not say which type implements it",
                    spec = spec.as_ref(),
                    function = function.as_ref(),
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
                write!(
                    f,
                    "cyclic conformance while proving '{target}: {}'",
                    spec.as_ref()
                )
            }
            Self::BlanketConformanceForeignSpec { spec_package } => {
                write!(
                    f,
                    "a blanket conform cannot implement a foreign spec from '{}'",
                    spec_package.as_ref()
                )
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
            Self::AsmRegNotOneRegisterOperand { r#type } => write!(
                f,
                "'reg' operand of type '{}' cannot occupy a single register",
                r#type
            ),
            Self::AsmConstNotComp => {
                write!(f, "'const' in 'asm' must name a 'comp' binding")
            }
            Self::AsmConstUnsupportedShape => write!(
                f,
                "'const' in 'asm' only supports values that convert deterministically to assembler text"
            ),
            Self::AsmUnknownBinding { text } => {
                write!(f, "'{text}' does not refer to any 'reg'/'const' descriptor")
            }
            Self::AsmAmbiguousBinding { text } => write!(
                f,
                "'{text}' is ambiguous: more than one descriptor infers this name"
            ),
            Self::NakedInlineConflict => {
                write!(f, "'@naked' cannot be combined with '@inline'")
            }
            Self::InvalidNakedBody => write!(
                f,
                "a '@naked' function's body must be exactly one 'asm' statement and nothing else"
            ),
            Self::AsmRegInNakedFunction => {
                write!(f, "'reg' is not allowed in a '@naked' function's 'asm'")
            }
        }
    }
}
