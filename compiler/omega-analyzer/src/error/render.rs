//! Turning a finding into a fully annotated diagnostic: a headline, a
//! primary label on the offending span, and -- only when it is *always*
//! true -- a help or note.

use super::*;

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
            Self::FieldNotVisible { field, base } => d
                .with_label(span, format!("`{field}` is not visible from this module"))
                .with_help(format!("mark the field `exposed`/`internal` on `{base}`, or bypass with `reveal`")),
            Self::MethodNotVisible { method, base } => d
                .with_label(span, format!("`{method}` is not visible from this module"))
                .with_help(format!("mark the method `exposed`/`internal` on `{base}`, or bypass with `reveal`")),
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
            Self::ConstSliceCannotBeMutable => d
                .with_label(span, "a compile-time slice cannot be mutable")
                .with_note("compile-time slice data is embedded directly in the binary, like a string literal")
                .with_help("write `&[...]` (without `mut`)"),
            Self::ConstSliceElementNotConstant => d
                .with_label(span, "not a literal constant")
                .with_note(
                    "a compile-time slice's contents are baked into the binary,\nso every element must be a literal (a number, string, bool, char, or nested compile-time slice)",
                ),
            Self::ConstSliceElementTypeMismatch { expected, found } => {
                d.with_label(span, format!("expected `{expected}`, found {found}"))
            }
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
            Self::UnresolvedLiteralGeneric { r#type, generics } => {
                let names = generics.iter().map(|g| format!("`{}`", g.as_ref())).collect::<Vec<_>>().join(", ");
                d.with_label(span, format!("cannot infer type argument(s) {names} of `{type}` here"))
                    .with_help(format!("write them explicitly, e.g. `{type}<...>`"))
            }
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
                    "each variant supplies this field's value as a compile-time constant,\nso header fields are currently limited to integers, floats, bool, char, `*str`, and immutable slices of those (`*[...]`)",
                ),
            Self::EnumVariantArgCount { expected, found, has_tag, .. } => {
                let what = if *has_tag { "the tag, then one value per header field" } else { "one value per header field" };
                d.with_label(span, format!("expected {expected} {}, found {found}", plural(*expected, "value")))
                    .with_note(format!("each variant's `(...)` must supply {what}, in header order"))
            }
            Self::EnumValueNotConstant => d
                .with_label(span, "not a literal constant")
                .with_note("a variant's tag and header values are baked in at the definition,\nso they must be literals (a number, string, bool, char, or `&[...]` compile-time slice)"),
            Self::EnumValueTypeMismatch { expected, found } => {
                d.with_label(span, format!("expected `{expected}`, found {found}"))
            }
            Self::DuplicateEnumTag { value, previous_variant, previous, .. } => d
                .with_label(span, format!("tag {value} used again here"))
                .with_secondary_label(*previous, format!("first used by variant '{}'", previous_variant.as_ref()))
                .with_note("the tag is how variants are told apart at runtime, so each variant needs its own"),
            Self::EnumFieldNameCollision { field, .. } => {
                let d = d.with_label(span, format!("`{}` already names a field of this enum", field.as_ref()));
                if field.as_ref() == "tag" {
                    d.with_note("`tag` is reserved: every enum value exposes its tag as `value.tag`")
                } else {
                    d.with_note("the tag, header fields, shared dynamic fields, and each variant's body fields are all accessed as `value.name`, so they share one namespace")
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
                .with_label(span, "this value's variant is not statically known here")
                .with_note(format!(
                    "'{}' belongs to variant '{}', which this value may or may not be;\nonly `tag`, the shared header fields, and the shared dynamic fields are always present",
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
                .with_note("the tag and header fields are per-variant constants; only a variant's own body fields and shared dynamic fields can be assigned"),
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
                .with_label(span, "this pattern covers values an earlier arm already covers")
                .with_secondary_label(*previous, "first covered here")
                .with_note("`match` has no first-match-wins rule -- every value must be covered by exactly one arm"),
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
                .with_note(
                    "casts are only supported between numeric types, pointers, \
                     the str/byte-slice family (*str, *[u8], *[i8]), and into a \
                     spec object (spec *Spec) when the source genuinely implements it",
                ),
            Self::CastToMutablePointer { from, to } => d
                .with_label(span, format!("cannot cast '{from}' to '{to}'"))
                .with_help("a cast can only target a mutable pointer/slice/str if the source is already mutable"),
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
            Self::AmbiguousSelfOverload { name, previous } => d
                .with_label(span, format!("`{}` differs from the other declaration only in how it receives `self`", name.as_ref()))
                .with_secondary_label(*previous, format!("`{}` first declared here", name.as_ref()))
                .with_help(
                    "a call site has no syntax to choose between receiving `self` by value and by pointer -- \
                     give these methods different names, or make another parameter differ too",
                ),
            Self::MissingSpecFunction { implementor, spec, spec_type_args, function } => d
                .with_label(
                    span,
                    format!("`{}` does not implement `{}`", implementor.as_ref(), generic_name(spec, spec_type_args)),
                )
                .with_help(format!(
                    "add a `{}` method to `{}`, or give `{}` a default implementation",
                    function.as_ref(),
                    implementor.as_ref(),
                    function.as_ref()
                )),
            Self::ConflictingSpecFunctions { name, first_spec, second_spec } => d
                .with_label(
                    span,
                    format!(
                        "`{}` and `{}` both require `{}`, with different signatures",
                        first_spec.as_ref(),
                        second_spec.as_ref(),
                        name.as_ref()
                    ),
                )
                .with_help(format!(
                    "give the implementing type its own `{}` method matching both",
                    name.as_ref()
                )),
            Self::AmbiguousSpecReturnType { function, first, second } => d
                .with_label(span, format!("`{}` returns both `{first}` and `{second}`", function.as_ref()))
                .with_help("a 'spec T' return type must resolve to exactly one concrete type across every exit point"),
            Self::SpecReturnTypeUnconstrained { function } => d
                .with_label(span, format!("cannot infer a concrete type for `{}`", function.as_ref()))
                .with_help("a 'spec T' return type needs at least one 'return' or tail expression to infer from"),
            Self::SpecReturnTypeNotSatisfied { function, r#type, spec, missing } => d
                .with_label(
                    span,
                    format!("`{}` returns `{type}`, which does not implement `{}`", function.as_ref(), spec.as_ref()),
                )
                .with_help(format!(
                    "`{type}` is missing: {}",
                    missing.iter().map(|m| m.as_ref()).collect::<Vec<_>>().join(", ")
                )),
            Self::ForLoopSourceNotIterable { r#type } => d
                .with_label(span, format!("`{type}` does not implement `ToIterator<T>` or `Iterator<T>`"))
                .with_help(format!(
                    "declare `{type} : ToIterator<T>` and implement `to_iterator`, \
                     or `{type} : Iterator<T>` and implement `next` directly"
                )),
            Self::SpecMethodTooHidden { implementor, spec, function, required, found } => d
                .with_label(
                    span,
                    format!(
                        "`{}` on `{}` is `{}`, but spec `{}` requires at least `{}`",
                        function.as_ref(),
                        implementor.as_ref(),
                        found,
                        spec.as_ref(),
                        required,
                    ),
                )
                .with_help(format!(
                    "mark `{}` on `{}` as `{}` (or more permissive)",
                    function.as_ref(),
                    implementor.as_ref(),
                    required
                )),
            Self::NoSuchSpecFunction { spec, function } => d.with_label(
                span,
                format!("no function `{}` on spec `{}`", function.as_ref(), spec.as_ref()),
            ),
            Self::SpecSelfMustBePointer { name } => d
                .with_label(span, format!("`{}` receives `self` by value", name.as_ref()))
                .with_help(
                    "spec functions must receive `self` by pointer (`*self`/`*mut self`) -- `spec *T` dynamic \
                     dispatch erases the concrete type down to a bare data pointer, which can't carry a \
                     by-value copy",
                ),
            Self::ExtensionOutsideCore { name } => d
                .with_label(span, format!("`{}` uses a `for` clause outside `core`", name.as_ref()))
                .with_help("`for` only has an effect on a spec declared inside the module tree rooted at `core`"),
            Self::ExtensionTargetNotAllowed { name } => d
                .with_label(span, format!("`{}` targets a type `for` doesn't support", name.as_ref()))
                .with_help(
                    "`for` may only target one of core's built-in scalar/`str` types, or `[T]` referencing this \
                     spec's own single generic parameter",
                ),
            Self::ExtensionSelfMustBePointer { name } => d
                .with_label(span, format!("`{}` receives `self` by value", name.as_ref()))
                .with_help(
                    "a `for [T]` function must receive `self` by pointer (`*self`/`*mut self`) -- by value, \
                     `self` has no length and can't be indexed safely",
                ),
            Self::DuplicateExtensionTarget { target, previous } => d
                .with_label(span, format!("another `for` block already targets `{target}`"))
                .with_secondary_label(*previous, format!("`{target}` first targeted here"))
                .with_help("only one `for` block is allowed per target type"),
            Self::UnknownAnnotation { name } => {
                d.with_label(span, format!("'@{}' is not a recognized annotation", name.as_ref()))
            }
            Self::AnnotationNotApplicable { name, found, allowed } => d
                .with_label(span, format!("cannot be used on {found}"))
                .with_note(format!(
                    "'@{}' only applies to {}",
                    name.as_ref(),
                    crate::annotations::item_kind_list(allowed)
                )),
            Self::DuplicateAnnotation { name } => {
                d.with_label(span, format!("'@{}' is already applied to this item", name.as_ref()))
            }
            Self::InvalidAnnotationArgs { name, reason } => {
                d.with_label(span, format!("'@{}' {reason}", name.as_ref()))
            }
            Self::ManglingDisabledOnGeneric => d
                .with_label(span, "cannot disable mangling on a generic function")
                .with_note(
                    "the compiler-generated instantiation suffix is the only thing keeping distinct instantiations from colliding on one symbol",
                ),
            Self::ManglingDisabledOnMethod => d
                .with_label(span, "cannot disable mangling on a struct/enum/union method")
                .with_help("only top-level functions can disable mangling for now"),
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
        TypeResolutionError::NotASpec(_) => d
            .with_label(span, "not a spec")
            .with_help("`spec *...`'s pointee must name a spec, e.g. `spec *Animal`"),
        TypeResolutionError::SpecNotObjectSafe(_) => d
            .with_label(span, "not object-safe")
            .with_help("use a generic bound (`T: ...`) or `spec T` static dispatch instead"),
        TypeResolutionError::SpecStaticNotAllowedHere(_) => d
            .with_label(span, "`spec ...` (static dispatch) is only allowed as a parameter type or a return type"),
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
        ResolveError::UnknownExtern(name) => with_label(d, "no such extern dependency".to_string())
            .with_help(format!("pass --extern={}:<path> on the command line", name.as_ref())),
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
        ResolveError::SpecDependencyCycle { spec, .. } => {
            with_label(d, format!("`{}` depends on itself, directly or through another spec's own dependency list", spec.as_ref()))
                .with_help("break the cycle by removing one of the dependencies, or depending on a common base spec instead")
        }
        ResolveError::SpecNotImplemented { missing, .. } => {
            with_label(d, "does not implement this spec".to_string()).with_note(format!(
                "missing: {}",
                missing.iter().map(Ident::as_ref).collect::<Vec<_>>().join(", ")
            ))
        }
        ResolveError::SpecReturnTypeRecursion { item, .. } => {
            with_label(d, format!("`{}` recursively depends on its own inferred return type", item.as_ref()))
                .with_help("give this function an explicit, concrete return type instead of 'spec T'")
        }
    }
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
