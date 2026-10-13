//! The one builder of `core::reflection` tables. `comp` evaluation and code
//! generation both expand a `Reflected` address through here, so a table can
//! never mean one thing at compile time and another at run time. The table
//! semantics live in `docs/language/reflection.md`.
//!
//! Field order and nested shapes are read by name from the `core`
//! declarations, which `core_contract::check_reflection` validated where the
//! program wrote `typeinfo`, so a missing name is an internal error.

use crate::checked::NumberValue;
use crate::layout::{self, EnumView};
use crate::resolved_type::{
    CallingConvention, CanonicalType, CompScalar, ConstValue, Reflected, ResolvedField,
    ResolvedGenericArg, ResolvedSpecApplication, ResolvedType,
};
use crate::type_store::{NominalRef, TypeStore};
use omega_hir::ModulePath;
use omega_parser::prelude::{Ident, Visibility};

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
    types: &TypeStore,
    reflected: &Reflected,
    pointee: &ResolvedType,
    pointer_bytes: u32,
) -> Result<ConstValue, &'static str> {
    match reflected {
        Reflected::TypeInfo { described, table } if pointee.widened() == *table => {
            if layout::require_complete(types, described).is_err() {
                return Err("reading reflection data of a type whose definition is incomplete");
            }
            Ok(type_info_value(types, described, table, pointer_bytes))
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
    types: &TypeStore,
    described: &ResolvedType,
    info_type: &ResolvedType,
    pointer_bytes: u32,
) -> ConstValue {
    let builder = Builder {
        types,
        info_type,
        pointer_bytes,
    };
    builder.type_info(&described.widened())
}

struct Builder<'a> {
    types: &'a TypeStore,
    info_type: &'a ResolvedType,
    pointer_bytes: u32,
}

struct Naming {
    name: String,
    path: ModulePath,
    generic_args: Vec<ResolvedGenericArg>,
}

impl Builder<'_> {
    fn type_ref(&self, described: &ResolvedType) -> ConstValue {
        type_info_ref(described, self.info_type)
    }

    fn type_info(&self, described: &ResolvedType) -> ConstValue {
        let naming = naming(described);
        let generic_arg_type = slice_item(&field_type(self.types, self.info_type, "generic_args"));
        let generic_args = naming
            .generic_args
            .iter()
            .map(|arg| self.generic_arg(&generic_arg_type, arg))
            .collect();
        let size = layout::total_bytes(self.types, described, self.pointer_bytes);
        let align = layout::type_alignment(self.types, described);
        build_struct(
            self.types,
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
            ResolvedGenericArg::Type(r#type) => build_variant(
                self.types,
                arg_type,
                "Type",
                vec![("type", self.type_ref(r#type))],
            ),
            ResolvedGenericArg::Comp(value) => build_variant(
                self.types,
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
                let width =
                    layout::total_bytes(self.types, &r#type.resolved(), self.pointer_bytes) * 8;
                truncate(*value as u64, width)
            }
            CompScalar::Bool(value) => u64::from(*value),
            CompScalar::Char(value) => u64::from(u32::from(*value)),
        }
    }

    fn kind(&self, described: &ResolvedType) -> ConstValue {
        let kind_type = field_type(self.types, self.info_type, "kind");
        let variant = |name, fields| build_variant(self.types, &kind_type, name, fields);
        let field_list = |variant_name: &str, field_name: &str| {
            slice_item(&variant_field_type(
                self.types,
                &kind_type,
                variant_name,
                field_name,
            ))
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
            ResolvedType::Struct(nominal) => {
                let struct_type = self.types.frozen().struct_facts(nominal);
                if struct_type.is_marker {
                    return variant("Marker", vec![]);
                }
                let info = field_list("Struct", "fields");
                let offsets =
                    layout::struct_layout(self.types, struct_type, self.pointer_bytes).byte_offsets;
                let fields = struct_type
                    .fields
                    .iter()
                    .zip(offsets)
                    .map(|(field, offset)| self.field_info_of(&info, field, offset))
                    .collect();
                variant("Struct", vec![("fields", ConstValue::Slice(fields))])
            }
            ResolvedType::Union(nominal) => {
                let info = field_list("Union", "fields");
                let fields = self
                    .types
                    .frozen()
                    .fields(nominal)
                    .iter()
                    .map(|field| self.field_info_of(&info, field, 0))
                    .collect();
                variant("Union", vec![("fields", ConstValue::Slice(fields))])
            }
            ResolvedType::Enum { nominal, .. } => self.named_enum(&kind_type, described, nominal),
            ResolvedType::AnonymousEnum { shape, .. } => {
                let view = EnumView::of_anonymous(shape);
                let info = variant_field_type(self.types, &kind_type, "AnonymousEnum", "tag_field");
                let member_type = field_list("AnonymousEnum", "members");
                let body_info = slice_item(&field_type(self.types, &member_type, "fields"));
                let members = shape
                    .members()
                    .iter()
                    .enumerate()
                    .map(|(index, member)| {
                        let offset = layout::enum_body_field_offset(
                            self.types,
                            &view,
                            index,
                            0,
                            self.pointer_bytes,
                        );
                        build_struct(
                            self.types,
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
                let convention_type =
                    variant_field_type(self.types, &kind_type, "Function", "convention");
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
                            build_variant(self.types, &convention_type, convention, vec![]),
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
                let primitive_type =
                    variant_field_type(self.types, &kind_type, "Primitive", "primitive");
                variant(
                    "Primitive",
                    vec![(
                        "primitive",
                        build_variant(
                            self.types,
                            &primitive_type,
                            primitive_name(primitive),
                            vec![],
                        ),
                    )],
                )
            }
        }
    }

    fn named_enum(
        &self,
        kind_type: &ResolvedType,
        described: &ResolvedType,
        nominal: &NominalRef,
    ) -> ConstValue {
        let enum_type = self.types.frozen().enum_facts(nominal);
        let view = EnumView::of_named(enum_type);
        let info = variant_field_type(self.types, kind_type, "Enum", "tag_field");
        let variant_type = slice_item(&variant_field_type(
            self.types, kind_type, "Enum", "variants",
        ));
        let header = enum_type
            .header
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let offset =
                    layout::enum_header_offset(self.types, &view, index, self.pointer_bytes);
                self.field_info_of(&info, field, offset)
            })
            .collect();
        let shared = enum_type
            .dynamic_fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let offset =
                    layout::enum_dynamic_field_offset(self.types, &view, index, self.pointer_bytes);
                self.field_info_of(&info, field, offset)
            })
            .collect();
        let tag_bits = layout::total_bytes(self.types, &enum_type.tag_type, self.pointer_bytes) * 8;
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
                            self.types,
                            &view,
                            variant_index,
                            index,
                            self.pointer_bytes,
                        );
                        self.field_info_of(&info, field, offset)
                    })
                    .collect();
                build_struct(
                    self.types,
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
            self.types,
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
        let spec = &*member.spec;
        let generic_arg_type = slice_item(&field_type(self.types, info, "generic_args"));
        build_struct(
            self.types,
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
        let visibility_type = field_type(self.types, info, "visibility");
        let visibility = match visibility {
            Visibility::Hidden => "Hidden",
            Visibility::Shared => "Shared",
            Visibility::Exposed => "Exposed",
        };
        build_struct(
            self.types,
            info,
            vec![
                ("name", ConstValue::Str(name.to_string())),
                ("offset", usize_value(u64::from(offset))),
                ("type", self.type_ref(r#type)),
                (
                    "visibility",
                    build_variant(self.types, &visibility_type, visibility, vec![]),
                ),
            ],
        )
    }

    fn tag_field(&self, info: &ResolvedType, view: &EnumView) -> ConstValue {
        let offset =
            layout::enum_prefix_layout(self.types, view, self.pointer_bytes).byte_offsets[0];
        self.field_info(info, "tag", offset, &view.tag_type, Visibility::Exposed)
    }
}

fn naming(described: &ResolvedType) -> Naming {
    let named = |name: &Ident, path: &ModulePath, generic_args: &[ResolvedGenericArg]| Naming {
        name: name.as_ref().to_string(),
        path: path.clone(),
        generic_args: generic_args.to_vec(),
    };
    match described {
        ResolvedType::Struct(nominal)
        | ResolvedType::Union(nominal)
        | ResolvedType::Enum { nominal, .. } => {
            named(&nominal.name, &nominal.module_path, &nominal.generic_args)
        }
        unnamed => Naming {
            name: CanonicalType(unnamed).to_string(),
            path: ModulePath::new([]),
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

fn path_value(path: &ModulePath) -> ConstValue {
    ConstValue::Slice(
        path.segments()
            .iter()
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

fn field_type(types: &TypeStore, owner: &ResolvedType, name: &str) -> ResolvedType {
    let ResolvedType::Struct(nominal) = owner else {
        missing(format_args!("'{owner}' as a struct"));
    };
    types
        .frozen()
        .fields(nominal)
        .iter()
        .find(|field| field.name.as_ref() == name)
        .map(|field| field.r#type.clone())
        .unwrap_or_else(|| missing(format_args!("'{owner}::{name}'")))
}

fn variant_field_type(
    types: &TypeStore,
    owner: &ResolvedType,
    variant: &str,
    name: &str,
) -> ResolvedType {
    let ResolvedType::Enum { nominal, .. } = owner else {
        missing(format_args!("'{owner}' as an enum"));
    };
    types
        .frozen()
        .enum_facts(nominal)
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

fn build_struct(
    types: &TypeStore,
    r#type: &ResolvedType,
    values: Vec<(&str, ConstValue)>,
) -> ConstValue {
    let ResolvedType::Struct(nominal) = r#type else {
        missing(format_args!("'{type}' as a struct"));
    };
    ConstValue::Struct(ordered(
        types.frozen().fields(nominal),
        nominal.name.as_ref(),
        values,
    ))
}

fn build_variant(
    types: &TypeStore,
    r#type: &ResolvedType,
    name: &str,
    values: Vec<(&str, ConstValue)>,
) -> ConstValue {
    let ResolvedType::Enum { nominal, .. } = r#type else {
        missing(format_args!("'{type}' as an enum"));
    };
    let enum_type = types.frozen().enum_facts(nominal);
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
