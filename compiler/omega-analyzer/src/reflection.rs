//! The one builder of `core::reflection` tables. `comp` evaluation and code
//! generation both expand a `Reflected` address through here, so a table can
//! never mean one thing at compile time and another at run time. The table
//! semantics live in `docs/language/reflection.md`.
//!
//! Field order and nested shapes are read by name from the trusted `core`
//! declarations rather than assumed, so a missing name is an internal error.

use crate::checked::NumberValue;
use crate::layout::{self, EnumView};
use crate::resolved_type::{
    CallingConvention, CanonicalType, CompScalar, ConstValue, Reflected, ResolvedField,
    ResolvedGenericArg, ResolvedSpecApplication, ResolvedType,
};
use omega_parser::prelude::{Ident, Visibility};

/// The absolute path of the table type `typeinfo<T>` addresses.
pub const TYPE_INFO_PATH: [&str; 3] = ["core", "reflection", "TypeInfo"];

pub fn type_info_path() -> Vec<Ident> {
    TYPE_INFO_PATH
        .map(|segment| Ident(segment.to_string()))
        .to_vec()
}

/// The address of `described`'s table, stored as `table`. Refinement never
/// changes storage, so a refined enum is described by its parent type.
pub fn type_info_ref(described: &ResolvedType, table: &ResolvedType) -> ConstValue {
    ConstValue::Reflected(Reflected::TypeInfo {
        described: described.widened(),
        table: table.clone(),
    })
}

/// What a `comp` dereference of a `Reflected` address reads through a
/// pointer to `pointee`.
pub fn deref(
    reflected: &Reflected,
    pointee: &ResolvedType,
    pointer_bytes: u32,
) -> Result<ConstValue, &'static str> {
    match reflected {
        Reflected::TypeInfo { described, table } if pointee.widened() == *table => {
            Ok(type_info_value(described, table, pointer_bytes))
        }
        Reflected::TypeInfo { .. } => {
            Err("reading reflection data through a pointer of another type")
        }
        Reflected::VariantPrototype { .. } => Err("reading a variant prototype"),
    }
}

/// The `TypeInfo` value describing `described`, shaped as `info_type`.
/// Every nested `*TypeInfo` or prototype is a `Reflected` leaf, so building
/// never recurses into the types this one refers to.
pub fn type_info_value(
    described: &ResolvedType,
    info_type: &ResolvedType,
    pointer_bytes: u32,
) -> ConstValue {
    let builder = Builder {
        info_type,
        pointer_bytes,
    };
    builder.type_info(&described.widened())
}

struct Builder<'a> {
    info_type: &'a ResolvedType,
    pointer_bytes: u32,
}

struct Naming {
    name: String,
    path: Vec<Ident>,
    generic_args: Vec<ResolvedGenericArg>,
}

impl Builder<'_> {
    fn type_ref(&self, described: &ResolvedType) -> ConstValue {
        type_info_ref(described, self.info_type)
    }

    fn type_info(&self, described: &ResolvedType) -> ConstValue {
        let naming = naming(described);
        let generic_arg_type = slice_item(&field_type(self.info_type, "generic_args"));
        let generic_args = naming
            .generic_args
            .iter()
            .map(|arg| self.generic_arg(&generic_arg_type, arg))
            .collect();
        let size = layout::total_bytes(described, self.pointer_bytes);
        let align = layout::type_alignment(described);
        build_struct(
            self.info_type,
            vec![
                ("name", ConstValue::Str(naming.name)),
                ("path", path_value(&naming.path)),
                ("generic_args", ConstValue::Slice(generic_args)),
                ("size", usize_value(u64::from(size))),
                ("align", usize_value(u64::from(align))),
                ("kind", self.kind(described)),
            ],
        )
    }

    fn generic_arg(&self, arg_type: &ResolvedType, arg: &ResolvedGenericArg) -> ConstValue {
        match arg {
            ResolvedGenericArg::Type(r#type) => {
                build_variant(arg_type, "Type", vec![("type", self.type_ref(r#type))])
            }
            ResolvedGenericArg::Comp(value) => build_variant(
                arg_type,
                "Comp",
                vec![
                    ("type", self.type_ref(&value.resolved_type())),
                    (
                        "bits",
                        ConstValue::Number(NumberValue::Unsigned(self.comp_bits(value))),
                    ),
                ],
            ),
        }
    }

    fn comp_bits(&self, value: &CompScalar) -> u64 {
        match value {
            CompScalar::Int { r#type, value } => {
                let width = layout::total_bytes(&r#type.resolved(), self.pointer_bytes) * 8;
                truncate(*value as u64, width)
            }
            CompScalar::Bool(value) => u64::from(*value),
            CompScalar::Char(value) => u64::from(u32::from(*value)),
        }
    }

    fn kind(&self, described: &ResolvedType) -> ConstValue {
        let kind_type = field_type(self.info_type, "kind");
        let variant = |name, fields| build_variant(&kind_type, name, fields);
        let field_list = |variant_name: &str, field_name: &str| {
            slice_item(&variant_field_type(&kind_type, variant_name, field_name))
        };
        match described {
            ResolvedType::Pointer { pointee, mutable } => variant(
                "Pointer",
                vec![
                    ("pointee", self.type_ref(pointee)),
                    ("mutable", ConstValue::Bool(*mutable)),
                ],
            ),
            ResolvedType::Array(element, mutable) => variant(
                "UnknownSizeArray",
                vec![
                    ("element", self.type_ref(element)),
                    ("mutable", ConstValue::Bool(*mutable)),
                ],
            ),
            ResolvedType::SizedArray(element, length) => variant(
                "Array",
                vec![
                    ("element", self.type_ref(element)),
                    ("length", usize_value(u64::from(*length))),
                ],
            ),
            ResolvedType::Slice { item, mutable } => variant(
                "Slice",
                vec![
                    ("element", self.type_ref(item)),
                    ("mutable", ConstValue::Bool(*mutable)),
                ],
            ),
            ResolvedType::Str { mutable } => {
                variant("Str", vec![("mutable", ConstValue::Bool(*mutable))])
            }
            ResolvedType::Struct(cell) => {
                let struct_type = cell.borrow();
                if struct_type.is_marker {
                    return variant("Marker", vec![]);
                }
                let info = field_list("Struct", "fields");
                let offsets = layout::struct_layout(&struct_type, self.pointer_bytes).byte_offsets;
                let fields = struct_type
                    .fields
                    .iter()
                    .zip(offsets)
                    .map(|(field, offset)| self.field_info_of(&info, field, offset))
                    .collect();
                variant("Struct", vec![("fields", ConstValue::Slice(fields))])
            }
            ResolvedType::Union(cell) => {
                let info = field_list("Union", "fields");
                let fields = cell
                    .borrow()
                    .fields
                    .iter()
                    .map(|field| self.field_info_of(&info, field, 0))
                    .collect();
                variant("Union", vec![("fields", ConstValue::Slice(fields))])
            }
            ResolvedType::Enum { cell, .. } => self.named_enum(&kind_type, described, cell),
            ResolvedType::AnonymousEnum { shape, .. } => {
                let view = EnumView::of_anonymous(shape);
                let info = variant_field_type(&kind_type, "AnonymousEnum", "tag_field");
                let member_type = field_list("AnonymousEnum", "members");
                let body_info = slice_item(&field_type(&member_type, "fields"));
                let members = shape
                    .members()
                    .iter()
                    .enumerate()
                    .map(|(index, member)| {
                        let offset =
                            layout::enum_body_field_offset(&view, index, 0, self.pointer_bytes);
                        build_struct(
                            &member_type,
                            vec![
                                ("name", ConstValue::Str(CanonicalType(member).to_string())),
                                (
                                    "tag",
                                    ConstValue::Number(NumberValue::Unsigned(index as u64)),
                                ),
                                ("prototype", prototype_ref(described, index)),
                                (
                                    "fields",
                                    ConstValue::Slice(vec![self.field_info(
                                        &body_info,
                                        "value",
                                        offset,
                                        member,
                                        Visibility::Exposed,
                                    )]),
                                ),
                            ],
                        )
                    })
                    .collect();
                variant(
                    "AnonymousEnum",
                    vec![
                        ("tag_field", self.tag_field(&info, &view)),
                        ("members", ConstValue::Slice(members)),
                    ],
                )
            }
            ResolvedType::Function(function) if function.self_mode.is_some() => {
                unreachable!("a receiver belongs to a method declaration, never to a value type")
            }
            ResolvedType::Function(function) => {
                let convention_type = variant_field_type(&kind_type, "Function", "convention");
                let convention = match function.calling_convention {
                    CallingConvention::Omega => "Omega",
                    CallingConvention::C => "C",
                    CallingConvention::SysV64 => "SysV64",
                };
                variant(
                    "Function",
                    vec![
                        (
                            "params",
                            ConstValue::Slice(
                                function.param_types().map(|t| self.type_ref(t)).collect(),
                            ),
                        ),
                        ("ret", self.type_ref(&function.return_type)),
                        ("variadic", ConstValue::Bool(function.is_variadic)),
                        (
                            "convention",
                            build_variant(&convention_type, convention, vec![]),
                        ),
                    ],
                )
            }
            ResolvedType::SpecObject { shape, mutable } => {
                let spec_info = field_list("SpecObject", "specs");
                let specs = shape
                    .members
                    .iter()
                    .map(|member| self.spec_info(&spec_info, member))
                    .collect();
                variant(
                    "SpecObject",
                    vec![
                        ("specs", ConstValue::Slice(specs)),
                        ("mutable", ConstValue::Bool(*mutable)),
                    ],
                )
            }
            ResolvedType::Spec(_) => unreachable!("a spec definition is never itself a value type"),
            primitive => {
                let primitive_type = variant_field_type(&kind_type, "Primitive", "primitive");
                variant(
                    "Primitive",
                    vec![(
                        "primitive",
                        build_variant(&primitive_type, primitive_name(primitive), vec![]),
                    )],
                )
            }
        }
    }

    fn named_enum(
        &self,
        kind_type: &ResolvedType,
        described: &ResolvedType,
        cell: &std::cell::RefCell<crate::resolved_type::ResolvedEnumType>,
    ) -> ConstValue {
        let enum_type = cell.borrow();
        let view = EnumView::of_named(&enum_type);
        let info = variant_field_type(kind_type, "Enum", "tag_field");
        let variant_type = slice_item(&variant_field_type(kind_type, "Enum", "variants"));
        let header = enum_type
            .header
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let offset = layout::enum_header_offset(&view, index, self.pointer_bytes);
                self.field_info_of(&info, field, offset)
            })
            .collect();
        let shared = enum_type
            .dynamic_fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let offset = layout::enum_dynamic_field_offset(&view, index, self.pointer_bytes);
                self.field_info_of(&info, field, offset)
            })
            .collect();
        let tag_bits = layout::total_bytes(&enum_type.tag_type, self.pointer_bytes) * 8;
        let variants = enum_type
            .variants
            .iter()
            .enumerate()
            .map(|(variant_index, variant)| {
                let fields = variant
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| {
                        let offset = layout::enum_body_field_offset(
                            &view,
                            variant_index,
                            index,
                            self.pointer_bytes,
                        );
                        self.field_info_of(&info, field, offset)
                    })
                    .collect();
                build_struct(
                    &variant_type,
                    vec![
                        ("name", ConstValue::Str(variant.name.as_ref().to_string())),
                        (
                            "tag",
                            ConstValue::Number(NumberValue::Unsigned(truncate(
                                number_bits(variant.tag),
                                tag_bits,
                            ))),
                        ),
                        ("prototype", prototype_ref(described, variant_index)),
                        ("fields", ConstValue::Slice(fields)),
                    ],
                )
            })
            .collect();
        build_variant(
            kind_type,
            "Enum",
            vec![
                ("tag_field", self.tag_field(&info, &view)),
                ("header", ConstValue::Slice(header)),
                ("shared", ConstValue::Slice(shared)),
                ("variants", ConstValue::Slice(variants)),
            ],
        )
    }

    fn spec_info(&self, info: &ResolvedType, member: &ResolvedSpecApplication) -> ConstValue {
        let spec = member.spec.borrow();
        let generic_arg_type = slice_item(&field_type(info, "generic_args"));
        build_struct(
            info,
            vec![
                ("name", ConstValue::Str(spec.name.as_ref().to_string())),
                ("path", path_value(&spec.module_path)),
                (
                    "generic_args",
                    ConstValue::Slice(
                        member
                            .spec_args
                            .iter()
                            .map(|arg| self.generic_arg(&generic_arg_type, arg))
                            .collect(),
                    ),
                ),
            ],
        )
    }

    fn field_info_of(&self, info: &ResolvedType, field: &ResolvedField, offset: u32) -> ConstValue {
        self.field_info(
            info,
            field.name.as_ref(),
            offset,
            &field.r#type,
            field.visibility,
        )
    }

    fn field_info(
        &self,
        info: &ResolvedType,
        name: &str,
        offset: u32,
        r#type: &ResolvedType,
        visibility: Visibility,
    ) -> ConstValue {
        let visibility_type = field_type(info, "visibility");
        let visibility = match visibility {
            Visibility::Hidden => "Hidden",
            Visibility::Shared => "Shared",
            Visibility::Exposed => "Exposed",
        };
        build_struct(
            info,
            vec![
                ("name", ConstValue::Str(name.to_string())),
                ("offset", usize_value(u64::from(offset))),
                ("type", self.type_ref(r#type)),
                (
                    "visibility",
                    build_variant(&visibility_type, visibility, vec![]),
                ),
            ],
        )
    }

    fn tag_field(&self, info: &ResolvedType, view: &EnumView) -> ConstValue {
        let offset = layout::enum_prefix_layout(view, self.pointer_bytes).byte_offsets[0];
        self.field_info(info, "tag", offset, &view.tag_type, Visibility::Exposed)
    }
}

fn naming(described: &ResolvedType) -> Naming {
    let named = |name: &Ident, path: &[Ident], generic_args: &[ResolvedGenericArg]| Naming {
        name: name.as_ref().to_string(),
        path: path.to_vec(),
        generic_args: generic_args.to_vec(),
    };
    match described {
        ResolvedType::Struct(cell) => {
            let s = cell.borrow();
            named(&s.name, &s.module_path, &s.generic_args)
        }
        ResolvedType::Union(cell) => {
            let u = cell.borrow();
            named(&u.name, &u.module_path, &u.generic_args)
        }
        ResolvedType::Enum { cell, .. } => {
            let e = cell.borrow();
            named(&e.name, &e.module_path, &e.generic_args)
        }
        unnamed => Naming {
            name: CanonicalType(unnamed).to_string(),
            path: Vec::new(),
            generic_args: Vec::new(),
        },
    }
}

fn primitive_name(primitive: &ResolvedType) -> &'static str {
    match primitive {
        ResolvedType::Void => "Void",
        ResolvedType::Never => "Never",
        ResolvedType::Bool => "Bool",
        ResolvedType::Char => "Char",
        ResolvedType::I8 => "I8",
        ResolvedType::I16 => "I16",
        ResolvedType::I32 => "I32",
        ResolvedType::I64 => "I64",
        ResolvedType::ISize => "ISize",
        ResolvedType::U8 => "U8",
        ResolvedType::U16 => "U16",
        ResolvedType::U32 => "U32",
        ResolvedType::U64 => "U64",
        ResolvedType::USize => "USize",
        ResolvedType::F32 => "F32",
        ResolvedType::F64 => "F64",
        other => unreachable!("'{other}' is not a primitive type"),
    }
}

fn prototype_ref(described: &ResolvedType, variant: usize) -> ConstValue {
    ConstValue::Reflected(Reflected::VariantPrototype {
        r#type: described.widened(),
        variant,
    })
}

fn path_value(path: &[Ident]) -> ConstValue {
    ConstValue::Slice(
        path.iter()
            .map(|segment| ConstValue::Str(segment.as_ref().to_string()))
            .collect(),
    )
}

fn usize_value(value: u64) -> ConstValue {
    ConstValue::Number(NumberValue::Unsigned(value))
}

fn number_bits(value: NumberValue) -> u64 {
    match value {
        NumberValue::Signed(value) => value as u64,
        NumberValue::Unsigned(value) => value,
        NumberValue::Float(_) => unreachable!("an enum tag is always an integer"),
    }
}

/// Two's-complement truncation to the low `width` bits.
fn truncate(bits: u64, width: u32) -> u64 {
    if width >= 64 {
        bits
    } else {
        bits & ((1u64 << width) - 1)
    }
}

fn missing(what: impl std::fmt::Display) -> ! {
    panic!("internal compiler error: 'core::reflection' does not declare {what}")
}

fn slice_item(r#type: &ResolvedType) -> ResolvedType {
    match r#type {
        ResolvedType::Slice { item, .. } => (**item).clone(),
        other => missing(format_args!("a slice where '{other}' is")),
    }
}

fn field_type(owner: &ResolvedType, name: &str) -> ResolvedType {
    let ResolvedType::Struct(cell) = owner else {
        missing(format_args!("'{owner}' as a struct"));
    };
    cell.borrow()
        .fields
        .iter()
        .find(|field| field.name.as_ref() == name)
        .map(|field| field.r#type.clone())
        .unwrap_or_else(|| missing(format_args!("'{owner}::{name}'")))
}

fn variant_field_type(owner: &ResolvedType, variant: &str, name: &str) -> ResolvedType {
    let ResolvedType::Enum { cell, .. } = owner else {
        missing(format_args!("'{owner}' as an enum"));
    };
    cell.borrow()
        .variants
        .iter()
        .find(|v| v.name.as_ref() == variant)
        .and_then(|v| v.fields.iter().find(|field| field.name.as_ref() == name))
        .map(|field| field.r#type.clone())
        .unwrap_or_else(|| missing(format_args!("'{owner}::{variant}::{name}'")))
}

/// Orders `values` by the declared fields, which must match them exactly.
fn ordered(
    declared: &[ResolvedField],
    owner: &str,
    mut values: Vec<(&str, ConstValue)>,
) -> Vec<ConstValue> {
    let ordered = declared
        .iter()
        .map(|field| {
            let position = values
                .iter()
                .position(|(name, _)| *name == field.name.as_ref())
                .unwrap_or_else(|| {
                    panic!(
                        "internal compiler error: the reflection builder has no value for '{owner}::{}'",
                        field.name.as_ref()
                    )
                });
            values.swap_remove(position).1
        })
        .collect();
    if let Some((name, _)) = values.first() {
        missing(format_args!("'{owner}::{name}'"));
    }
    ordered
}

fn build_struct(r#type: &ResolvedType, values: Vec<(&str, ConstValue)>) -> ConstValue {
    let ResolvedType::Struct(cell) = r#type else {
        missing(format_args!("'{type}' as a struct"));
    };
    let struct_type = cell.borrow();
    ConstValue::Struct(ordered(
        &struct_type.fields,
        struct_type.name.as_ref(),
        values,
    ))
}

fn build_variant(r#type: &ResolvedType, name: &str, values: Vec<(&str, ConstValue)>) -> ConstValue {
    let ResolvedType::Enum { cell, .. } = r#type else {
        missing(format_args!("'{type}' as an enum"));
    };
    let enum_type = cell.borrow();
    let Some((variant_index, variant)) = enum_type
        .variants
        .iter()
        .enumerate()
        .find(|(_, v)| v.name.as_ref() == name)
    else {
        missing(format_args!("'{type}::{name}'"));
    };
    ConstValue::Enum {
        variant_index,
        tag: variant.tag,
        header: variant.header_values.clone(),
        dynamic_fields: Vec::new(),
        fields: ordered(&variant.fields, name, values),
    }
}

#[cfg(test)]
mod tests;
