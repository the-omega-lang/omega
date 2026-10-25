//! What the compiler relies on `core` to declare, and the shape it relies on.
//!
//! The compiler reaches these declarations by absolute path, never through a
//! program's own scope, and checks each feature's declarations when a program
//! first uses that feature. A `core` that omits a feature still compiles every
//! program that does not use it; one that declares it differently is reported
//! as [`AnalysisErrorKind::CoreSupportUnavailable`](crate::error::AnalysisErrorKind)
//! rather than miscompiled. The full list is in
//! `docs/architecture/runtime-and-platform.md`.

use crate::resolved_type::{GapFunction, ResolvedField, ResolvedType};
use crate::runtime_checks::{PanicInfoField, PanicSupport};
use crate::type_store::{EnumFacts, SpecFacts, StructFacts};
use omega_hir::{ItemPath, ModulePath};
use omega_parser::prelude::{Ident, SelfMode, Type};
use std::fmt;

/// A language feature the compiler implements on top of `core` declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreFeature {
    RuntimeChecks,
    Ranges,
    Iteration,
    TryOperator,
    Reflection,
}

impl fmt::Display for CoreFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RuntimeChecks => "runtime checks",
            Self::Ranges => "range expressions",
            Self::Iteration => "'for' loops",
            Self::TryOperator => "the '?' operator",
            Self::Reflection => "'typeinfo'",
        })
    }
}

pub const PANIC_MODULE: [&str; 2] = ["core", "panic"];
pub const PANIC_INFO: &str = "PanicInfo";
pub const PANIC_HANDLER: &str = "PanicHandler";
pub const PANIC_HANDLER_FUNCTION: &str = "panic";

const RANGE_MODULE: [&str; 2] = ["core", "range"];
const ITERATOR_MODULE: [&str; 2] = ["core", "iterator"];
const OPTION_MODULE: [&str; 2] = ["core", "option"];
const RESULT_MODULE: [&str; 2] = ["core", "result"];
const REFLECTION_MODULE: [&str; 2] = ["core", "reflection"];

fn item(module: [&str; 2], name: &str) -> ItemPath {
    ModulePath::from_names(&module).join(Ident(name.to_string()))
}

fn is_item(path: &ModulePath, name: &Ident, module: [&str; 2], expected: &str) -> bool {
    path.is(&module) && name.as_ref() == expected
}

pub fn range_path() -> ItemPath {
    item(RANGE_MODULE, "Range")
}

pub fn bounded_path() -> ItemPath {
    item(RANGE_MODULE, "Bounded")
}

pub fn iterator_path() -> ItemPath {
    item(ITERATOR_MODULE, "Iterator")
}

pub fn to_iterator_path() -> ItemPath {
    item(ITERATOR_MODULE, "ToIterator")
}

pub fn type_info_path() -> ItemPath {
    item(REFLECTION_MODULE, "TypeInfo")
}

pub fn is_range(path: &ModulePath, name: &Ident) -> bool {
    is_item(path, name, RANGE_MODULE, "Range")
}

pub fn is_option(path: &ModulePath, name: &Ident) -> bool {
    is_item(path, name, OPTION_MODULE, "Option")
}

pub fn is_result(path: &ModulePath, name: &Ident) -> bool {
    is_item(path, name, RESULT_MODULE, "Result")
}

/// `Range<element>`: exactly `start`, `end`, `inclusive`, in that order,
/// because a range literal is built by field index.
pub fn check_range(fields: &[ResolvedField], element: &ResolvedType) -> Result<(), String> {
    let expected = [
        ("start", element.clone()),
        ("end", element.clone()),
        ("inclusive", ResolvedType::Bool),
    ];
    let matches = fields.len() == expected.len()
        && fields
            .iter()
            .zip(&expected)
            .all(|(field, (name, r#type))| field.name.as_ref() == *name && field.r#type == *r#type);
    if matches {
        Ok(())
    } else {
        Err(format!(
            "'{}' must declare exactly 'start: T', 'end: T' and 'inclusive: bool', in that order",
            range_path()
        ))
    }
}

/// `Bounded`: receiverless, parameterless `min` and `max` returning `Self`.
pub fn check_bounded(spec: &SpecFacts) -> Result<(), String> {
    for name in ["min", "max"] {
        let declared = spec
            .functions
            .iter()
            .find(|(function, _)| function.as_ref() == name)
            .is_some_and(|(_, raw)| {
                raw.self_mode.is_none()
                    && raw.params.is_empty()
                    && raw.generics.is_empty()
                    && matches!(&raw.return_type, Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "Self")
            });
        if !declared {
            return Err(format!(
                "'{}' must declare '{name}() => Self'",
                bounded_path()
            ));
        }
    }
    Ok(())
}

/// `Iterator<T>`: one type parameter and a `next(^self)` requirement. Its
/// return type is checked where a loop calls it, with [`check_option`].
pub fn check_iterator(spec: &SpecFacts) -> Result<(), String> {
    check_cursor_spec(spec, &iterator_path(), "next", SelfMode::MutPointer)
}

/// `ToIterator<T>`: one type parameter and a `to_iterator(*self)` requirement.
pub fn check_to_iterator(spec: &SpecFacts) -> Result<(), String> {
    check_cursor_spec(spec, &to_iterator_path(), "to_iterator", SelfMode::Pointer)
}

fn check_cursor_spec(
    spec: &SpecFacts,
    path: &ItemPath,
    function: &str,
    receiver: SelfMode,
) -> Result<(), String> {
    let declared = spec.generics.len() == 1
        && spec.functions.iter().any(|(name, raw)| {
            name.as_ref() == function
                && raw.self_mode == Some(receiver)
                && raw.generics.is_empty()
                // The receiver is the only parameter.
                && raw.params.len() == 1
        });
    if declared {
        Ok(())
    } else {
        let receiver = if receiver == SelfMode::MutPointer {
            "^self"
        } else {
            "*self"
        };
        Err(format!(
            "'{path}' must take one type parameter and declare '{function}({receiver})'"
        ))
    }
}

/// `Option<T>`: exactly `None` and then `Some { value }`. A `for` loop reads
/// the variants by position and `?` by name.
pub fn check_option(facts: &EnumFacts) -> Result<(), String> {
    match facts.variants.as_slice() {
        [none, some]
            if none.name.as_ref() == "None"
                && none.fields.is_empty()
                && some.name.as_ref() == "Some"
                && field_names(&some.fields) == ["value"] =>
        {
            Ok(())
        }
        _ => Err(format!(
            "'{}' must declare exactly 'None' and then 'Some {{ value }}'",
            item(OPTION_MODULE, "Option")
        )),
    }
}

/// `Result<T, E>`: exactly `Ok { value }` and `Err { error }`.
pub fn check_result(facts: &EnumFacts) -> Result<(), String> {
    let variant = |name: &str| facts.variants.iter().find(|v| v.name.as_ref() == name);
    match (facts.variants.len(), variant("Ok"), variant("Err")) {
        (2, Some(ok), Some(err))
            if field_names(&ok.fields) == ["value"] && field_names(&err.fields) == ["error"] =>
        {
            Ok(())
        }
        _ => Err(format!(
            "'{}' must declare exactly 'Ok {{ value }}' and 'Err {{ error }}'",
            item(RESULT_MODULE, "Result")
        )),
    }
}

fn field_names(fields: &[ResolvedField]) -> Vec<&str> {
    fields.iter().map(|field| field.name.as_ref()).collect()
}

/// Validates `core::panic` once its two declarations are resolved: the
/// `PanicInfo` struct (`info`) and the `PanicHandler` gap's `panic` function.
pub fn panic_support(
    info_type: ResolvedType,
    info: &StructFacts,
    handler: Option<&GapFunction>,
) -> Result<PanicSupport, String> {
    let info_path = item(PANIC_MODULE, PANIC_INFO);
    let handler_path = item(PANIC_MODULE, PANIC_HANDLER);
    let field = |name: &str, expected: ResolvedType| {
        let index = info
            .fields
            .iter()
            .position(|f| f.name.as_ref() == name)
            .ok_or_else(|| format!("'{info_path}' declares no '{name}' field"))?;
        let found = info.fields[index].r#type.clone();
        if found != expected {
            return Err(format!(
                "'{info_path}::{name}' must be '{expected}', found '{found}'"
            ));
        }
        Ok(PanicInfoField {
            index,
            r#type: found,
        })
    };
    let str_pointer = ResolvedType::Str { mutable: false };
    let source_file = field("source_file", str_pointer.clone())?;
    let line = field("line", ResolvedType::U32)?;
    let column = field("column", ResolvedType::U32)?;
    let message = field("message", str_pointer)?;

    let Some(handler) = handler else {
        return Err(format!(
            "gap '{handler_path}' declares no '{PANIC_HANDLER_FUNCTION}' function"
        ));
    };
    let signature = &handler.fn_type;
    let expected_param = ResolvedType::Pointer {
        pointee: Box::new(info_type.clone()),
        mutable: false,
    };
    match signature.params.as_slice() {
        [param] if param.r#type == expected_param => {}
        _ => {
            return Err(format!(
                "'{handler_path}::{PANIC_HANDLER_FUNCTION}' must take exactly one \
                 '*{PANIC_INFO}' parameter"
            ));
        }
    }
    if *signature.return_type != ResolvedType::Never || signature.is_variadic {
        return Err(format!(
            "'{handler_path}::{PANIC_HANDLER_FUNCTION}' must be a non-variadic '=> never' function"
        ));
    }
    Ok(PanicSupport {
        handler_decl_id: handler.decl_id,
        handler_fn_type: signature.clone(),
        info_type,
        source_file,
        line,
        column,
        message,
    })
}

/// The type of one reflection table field.
#[derive(Clone, Copy)]
enum Shape {
    /// `*str`
    Str,
    /// `*[]*str`
    Strs,
    Bool,
    U64,
    USize,
    /// `*u8`
    Bytes,
    /// `*TypeInfo`
    Info,
    /// `*[]*TypeInfo`
    Infos,
    /// A `core::reflection` declaration held by value.
    Value(&'static str),
    /// `*[]` of a `core::reflection` declaration.
    Values(&'static str),
}

type Fields = &'static [(&'static str, Shape)];

enum Declaration {
    Struct(Fields),
    /// Variants by name; a declaration may add variants the compiler never
    /// builds.
    Enum(&'static [(&'static str, Fields)]),
}

use Shape::*;

const FIELD_LIST: Fields = &[("fields", Values("FieldInfo"))];

/// Every declaration the reflection builder reads, with exactly the fields it
/// fills.
const REFLECTION: &[(&str, Declaration)] = &[
    (
        "TypeInfo",
        Declaration::Struct(&[
            ("name", Str),
            ("path", Strs),
            ("generic_args", Values("GenericArg")),
            ("size", USize),
            ("align", USize),
            ("kind", Value("TypeKind")),
        ]),
    ),
    (
        "GenericArg",
        Declaration::Enum(&[
            ("Type", &[("type", Info)]),
            ("Comp", &[("type", Info), ("bits", U64)]),
        ]),
    ),
    (
        "FieldInfo",
        Declaration::Struct(&[
            ("name", Str),
            ("offset", USize),
            ("type", Info),
            ("visibility", Value("Visibility")),
        ]),
    ),
    (
        "Visibility",
        Declaration::Enum(&[("Hidden", &[]), ("Shared", &[]), ("Exposed", &[])]),
    ),
    (
        "Primitive",
        Declaration::Enum(&[
            ("Void", &[]),
            ("Never", &[]),
            ("Bool", &[]),
            ("Char", &[]),
            ("I8", &[]),
            ("I16", &[]),
            ("I32", &[]),
            ("I64", &[]),
            ("ISize", &[]),
            ("U8", &[]),
            ("U16", &[]),
            ("U32", &[]),
            ("U64", &[]),
            ("USize", &[]),
            ("F32", &[]),
            ("F64", &[]),
        ]),
    ),
    (
        "CallingConvention",
        Declaration::Enum(&[("Omega", &[]), ("C", &[])]),
    ),
    (
        "SpecInfo",
        Declaration::Struct(&[
            ("name", Str),
            ("path", Strs),
            ("generic_args", Values("GenericArg")),
        ]),
    ),
    (
        "VariantInfo",
        Declaration::Struct(&[
            ("name", Str),
            ("tag", U64),
            ("prototype", Bytes),
            ("fields", Values("FieldInfo")),
        ]),
    ),
    (
        "TypeKind",
        Declaration::Enum(&[
            ("Primitive", &[("primitive", Value("Primitive"))]),
            ("Pointer", &[("pointee", Info), ("mutable", Bool)]),
            ("UnknownSizeArray", &[("element", Info), ("mutable", Bool)]),
            ("Array", &[("element", Info), ("length", USize)]),
            ("Slice", &[("element", Info), ("mutable", Bool)]),
            ("Str", &[("mutable", Bool)]),
            ("Struct", FIELD_LIST),
            ("Union", FIELD_LIST),
            ("Marker", &[]),
            (
                "Enum",
                &[
                    ("tag_field", Value("FieldInfo")),
                    ("header", Values("FieldInfo")),
                    ("shared", Values("FieldInfo")),
                    ("variants", Values("VariantInfo")),
                ],
            ),
            (
                "AnonymousEnum",
                &[
                    ("tag_field", Value("FieldInfo")),
                    ("members", Values("VariantInfo")),
                ],
            ),
            (
                "Function",
                &[
                    ("params", Infos),
                    ("ret", Info),
                    ("variadic", Bool),
                    ("convention", Value("CallingConvention")),
                ],
            ),
            (
                "SpecObject",
                &[("specs", Values("SpecInfo")), ("mutable", Bool)],
            ),
        ]),
    ),
];

/// The `core::reflection` declarations [`check_reflection`] needs resolved.
pub fn reflection_paths() -> impl Iterator<Item = ItemPath> {
    REFLECTION
        .iter()
        .map(|(name, _)| item(REFLECTION_MODULE, name))
}

/// The facts of one resolved `core::reflection` declaration.
pub enum ReflectionFacts<'a> {
    Struct(&'a [ResolvedField]),
    Enum(&'a EnumFacts),
}

/// Validates every `core::reflection` declaration against the shape the
/// reflection builder fills. `facts` reads a declaration by name, `None` when
/// `core` does not declare it as a struct or enum.
pub fn check_reflection<'a>(
    mut facts: impl FnMut(&str) -> Option<ReflectionFacts<'a>>,
) -> Result<(), String> {
    for (name, declaration) in REFLECTION {
        let path = item(REFLECTION_MODULE, name);
        match (declaration, facts(name)) {
            (Declaration::Struct(expected), Some(ReflectionFacts::Struct(fields))) => {
                check_fields(&path.to_string(), fields, expected)?;
            }
            (Declaration::Enum(variants), Some(ReflectionFacts::Enum(facts))) => {
                for (variant, expected) in *variants {
                    let Some(declared) =
                        facts.variants.iter().find(|v| v.name.as_ref() == *variant)
                    else {
                        return Err(format!("'{path}' declares no variant '{variant}'"));
                    };
                    check_fields(&format!("{path}::{variant}"), &declared.fields, expected)?;
                }
            }
            (Declaration::Struct(_), _) => return Err(format!("'{path}' must be a struct")),
            (Declaration::Enum(_), _) => return Err(format!("'{path}' must be an enum")),
        }
    }
    Ok(())
}

fn check_fields(owner: &str, fields: &[ResolvedField], expected: Fields) -> Result<(), String> {
    for field in fields {
        let Some((_, shape)) = expected
            .iter()
            .find(|(name, _)| *name == field.name.as_ref())
        else {
            return Err(format!(
                "'{owner}' declares a field '{}' the compiler does not fill",
                field.name.as_ref()
            ));
        };
        if !shape_matches(*shape, &field.r#type) {
            return Err(format!(
                "'{owner}::{}' must be '{}', found '{}'",
                field.name.as_ref(),
                shape_name(*shape),
                field.r#type
            ));
        }
    }
    if let Some((missing, _)) = expected
        .iter()
        .find(|(name, _)| !fields.iter().any(|field| field.name.as_ref() == *name))
    {
        return Err(format!("'{owner}' declares no field '{missing}'"));
    }
    Ok(())
}

fn is_reflection(r#type: &ResolvedType, name: &str) -> bool {
    let nominal = match r#type {
        ResolvedType::Struct(nominal) => nominal,
        ResolvedType::Enum {
            nominal,
            variant: None,
        } => nominal,
        _ => return false,
    };
    nominal.generic_args.is_empty()
        && is_item(&nominal.module_path, &nominal.name, REFLECTION_MODULE, name)
}

fn shape_matches(shape: Shape, r#type: &ResolvedType) -> bool {
    let immutable_slice = |r#type: &ResolvedType| match r#type {
        ResolvedType::Slice {
            item,
            mutable: false,
        } => Some((**item).clone()),
        _ => None,
    };
    let immutable_pointer = |r#type: &ResolvedType| match r#type {
        ResolvedType::Pointer {
            pointee,
            mutable: false,
        } => Some((**pointee).clone()),
        _ => None,
    };
    match shape {
        Str => *r#type == ResolvedType::Str { mutable: false },
        Strs => immutable_slice(r#type) == Some(ResolvedType::Str { mutable: false }),
        Bool => *r#type == ResolvedType::Bool,
        U64 => *r#type == ResolvedType::U64,
        USize => *r#type == ResolvedType::USize,
        Bytes => immutable_pointer(r#type) == Some(ResolvedType::U8),
        Info => {
            immutable_pointer(r#type).is_some_and(|pointee| is_reflection(&pointee, "TypeInfo"))
        }
        Infos => immutable_slice(r#type)
            .and_then(|item| immutable_pointer(&item))
            .is_some_and(|pointee| is_reflection(&pointee, "TypeInfo")),
        Value(name) => is_reflection(r#type, name),
        Values(name) => immutable_slice(r#type).is_some_and(|item| is_reflection(&item, name)),
    }
}

fn shape_name(shape: Shape) -> String {
    match shape {
        Str => "*str".into(),
        Strs => "*[]*str".into(),
        Bool => "bool".into(),
        U64 => "u64".into(),
        USize => "usize".into(),
        Bytes => "*u8".into(),
        Info => "*TypeInfo".into(),
        Infos => "*[]*TypeInfo".into(),
        Value(name) => name.into(),
        Values(name) => format!("*[]{name}"),
    }
}
