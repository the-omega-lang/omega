use super::*;
use crate::annotations::Layout;
use crate::resolved_type::{
    CompIntType, ResolvedAnonymousEnum, ResolvedEnumType, ResolvedEnumVariant, ResolvedStructType,
};
use omega_hir::ids::{HirId, ModuleId};
use std::cell::RefCell;
use std::rc::Rc;

const POINTER_BYTES: u32 = 8;

fn id(n: u32) -> HirId {
    HirId {
        module: ModuleId(0),
        local: n,
    }
}

fn ident(name: &str) -> Ident {
    Ident(name.to_string())
}

fn field(name: &str, r#type: ResolvedType) -> ResolvedField {
    ResolvedField::new(ident(name), r#type, Visibility::Exposed)
}

fn structure(n: u32, name: &str, fields: Vec<ResolvedField>, layout: Layout) -> ResolvedType {
    ResolvedType::Struct(Rc::new(RefCell::new(ResolvedStructType {
        id: id(n),
        name: ident(name),
        module_path: vec![ident("app")],
        generic_args: vec![],
        fields,
        functions: vec![],
        layout,
        suppress: vec![],
        is_marker: false,
    })))
}

struct Variant {
    name: &'static str,
    tag: NumberValue,
    header_values: Vec<ConstValue>,
    fields: Vec<ResolvedField>,
}

fn variant(name: &'static str, tag: u64, fields: Vec<ResolvedField>) -> Variant {
    Variant {
        name,
        tag: NumberValue::Unsigned(tag),
        header_values: vec![],
        fields,
    }
}

fn enumeration(
    n: u32,
    name: &str,
    tag_type: ResolvedType,
    header: Vec<ResolvedField>,
    dynamic_fields: Vec<ResolvedField>,
    variants: Vec<Variant>,
) -> ResolvedType {
    ResolvedType::Enum {
        cell: Rc::new(RefCell::new(ResolvedEnumType {
            id: id(n),
            name: ident(name),
            module_path: vec![ident("app")],
            generic_args: vec![],
            tag_type,
            header,
            dynamic_fields,
            variants: variants
                .into_iter()
                .map(|v| ResolvedEnumVariant {
                    name: ident(v.name),
                    tag: v.tag,
                    header_values: v.header_values,
                    fields: v.fields,
                })
                .collect(),
            functions: vec![],
            layout: Layout::default(),
            suppress: vec![],
        })),
        variant: None,
    }
}

fn unit_enum(n: u32, name: &str, variants: &[&'static str]) -> ResolvedType {
    let variants = variants
        .iter()
        .enumerate()
        .map(|(index, name)| variant(name, index as u64, vec![]))
        .collect();
    enumeration(n, name, ResolvedType::U32, vec![], vec![], variants)
}

fn slice(item: ResolvedType) -> ResolvedType {
    ResolvedType::Slice {
        item: Box::new(item),
        mutable: false,
    }
}

/// A stand-in for `core::reflection`'s declarations. The builder never looks
/// through a `*TypeInfo`, so those fields can point at anything.
fn info_type() -> ResolvedType {
    let opaque = ResolvedType::Pointer {
        pointee: Box::new(ResolvedType::U8),
        mutable: false,
    };
    let str = ResolvedType::Str { mutable: false };
    let visibility = unit_enum(10, "Visibility", &["Hidden", "Shared", "Exposed"]);
    let primitive = unit_enum(
        11,
        "Primitive",
        &[
            "Void", "Never", "Bool", "Char", "I8", "I16", "I32", "I64", "ISize", "U8", "U16",
            "U32", "U64", "USize", "F32", "F64",
        ],
    );
    let field_info = structure(
        12,
        "FieldInfo",
        vec![
            field("name", str.clone()),
            field("offset", ResolvedType::USize),
            field("type", opaque.clone()),
            field("visibility", visibility),
        ],
        Layout::default(),
    );
    let fields = slice(field_info.clone());
    let variant_info = structure(
        13,
        "VariantInfo",
        vec![
            field("name", str.clone()),
            field("tag", ResolvedType::U64),
            field("prototype", opaque.clone()),
            field("fields", fields.clone()),
        ],
        Layout::default(),
    );
    let generic_arg = enumeration(
        14,
        "GenericArg",
        ResolvedType::U32,
        vec![],
        vec![],
        vec![
            variant("Type", 0, vec![field("type", opaque.clone())]),
            variant(
                "Comp",
                1,
                vec![
                    field("type", opaque.clone()),
                    field("bits", ResolvedType::U64),
                ],
            ),
        ],
    );
    let element = |extra: (&str, ResolvedType)| {
        vec![field("element", opaque.clone()), field(extra.0, extra.1)]
    };
    let mutable = || ("mutable", ResolvedType::Bool);
    let kind = enumeration(
        15,
        "TypeKind",
        ResolvedType::U32,
        vec![],
        vec![],
        vec![
            variant("Primitive", 0, vec![field("primitive", primitive)]),
            variant(
                "Pointer",
                1,
                vec![
                    field("pointee", opaque.clone()),
                    field("mutable", ResolvedType::Bool),
                ],
            ),
            variant("UnknownSizeArray", 2, element(mutable())),
            variant("Array", 3, element(("length", ResolvedType::USize))),
            variant("Slice", 4, element(mutable())),
            variant("Str", 5, vec![field("mutable", ResolvedType::Bool)]),
            variant("Struct", 6, vec![field("fields", fields.clone())]),
            variant("Union", 7, vec![field("fields", fields.clone())]),
            variant("Marker", 8, vec![]),
            variant(
                "Enum",
                9,
                vec![
                    field("tag_field", field_info.clone()),
                    field("header", fields.clone()),
                    field("shared", fields.clone()),
                    field("variants", slice(variant_info.clone())),
                ],
            ),
            variant(
                "AnonymousEnum",
                10,
                vec![
                    field("tag_field", field_info),
                    field("members", slice(variant_info)),
                ],
            ),
            variant("Function", 11, vec![]),
            variant("SpecObject", 12, vec![]),
        ],
    );
    structure(
        16,
        "TypeInfo",
        vec![
            field("name", str.clone()),
            field("path", slice(str)),
            field("generic_args", slice(generic_arg)),
            field("size", ResolvedType::USize),
            field("align", ResolvedType::USize),
            field("kind", kind),
        ],
        Layout::default(),
    )
}

fn describe(described: &ResolvedType) -> ConstValue {
    type_info_value(described, &info_type(), POINTER_BYTES)
}

/// Reads a struct's field, or the body field of an enum value, by name.
fn get<'a>(value: &'a ConstValue, owner: &ResolvedType, name: &str) -> &'a ConstValue {
    match (value, owner) {
        (ConstValue::Struct(values), ResolvedType::Struct(cell)) => {
            let index = cell
                .borrow()
                .fields
                .iter()
                .position(|f| f.name.as_ref() == name)
                .unwrap_or_else(|| panic!("no field {name}"));
            &values[index]
        }
        (
            ConstValue::Enum {
                variant_index,
                fields,
                ..
            },
            ResolvedType::Enum { cell, .. },
        ) => {
            let index = cell.borrow().variants[*variant_index]
                .fields
                .iter()
                .position(|f| f.name.as_ref() == name)
                .unwrap_or_else(|| panic!("no body field {name}"));
            &fields[index]
        }
        _ => panic!("cannot read {name} from {value:?}"),
    }
}

fn number(value: &ConstValue) -> u64 {
    match value {
        ConstValue::Number(NumberValue::Unsigned(n)) => *n,
        other => panic!("not an unsigned number: {other:?}"),
    }
}

fn text(value: &ConstValue) -> &str {
    match value {
        ConstValue::Str(s) => s,
        other => panic!("not a string: {other:?}"),
    }
}

fn elements(value: &ConstValue) -> &[ConstValue] {
    match value {
        ConstValue::Slice(v) => v,
        other => panic!("not a slice: {other:?}"),
    }
}

fn kind_name(info: &ConstValue) -> String {
    let info_type = info_type();
    let kind_type = field_type(&info_type, "kind");
    let ConstValue::Enum { variant_index, .. } = get(info, &info_type, "kind") else {
        panic!("kind is not an enum value");
    };
    let ResolvedType::Enum { cell, .. } = kind_type else {
        unreachable!()
    };
    cell.borrow().variants[*variant_index]
        .name
        .as_ref()
        .to_string()
}

/// `(name, offset)` of every `FieldInfo` in a slice.
fn field_offsets(fields: &ConstValue) -> Vec<(String, u64)> {
    let field_info = slice_item(&variant_field_type(
        &field_type(&info_type(), "kind"),
        "Struct",
        "fields",
    ));
    elements(fields)
        .iter()
        .map(|f| {
            (
                text(get(f, &field_info, "name")).to_string(),
                number(get(f, &field_info, "offset")),
            )
        })
        .collect()
}

fn kind_field<'a>(info: &'a ConstValue, name: &str) -> &'a ConstValue {
    let info_type = info_type();
    get(
        get(info, &info_type, "kind"),
        &field_type(&info_type, "kind"),
        name,
    )
}

fn size_and_align(info: &ConstValue) -> (u64, u64) {
    let info_type = info_type();
    (
        number(get(info, &info_type, "size")),
        number(get(info, &info_type, "align")),
    )
}

#[test]
fn a_packed_struct_has_unpadded_offsets() {
    let s = structure(
        1,
        "Packed",
        vec![
            field("a", ResolvedType::U8),
            field("b", ResolvedType::U32),
            field("c", ResolvedType::U16),
        ],
        Layout::default(),
    );
    let info = describe(&s);

    assert_eq!(kind_name(&info), "Struct");
    assert_eq!(size_and_align(&info), (7, 1));
    assert_eq!(
        field_offsets(kind_field(&info, "fields")),
        vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 5)]
    );
    assert_eq!(text(get(&info, &info_type(), "name")), "Packed");
    let path: Vec<&str> = elements(get(&info, &info_type(), "path"))
        .iter()
        .map(text)
        .collect();
    assert_eq!(path, ["app"]);
}

#[test]
fn an_aligned_field_is_placed_and_padded_by_the_layout_rules() {
    let inner = structure(
        1,
        "Inner",
        vec![field("x", ResolvedType::U8)],
        Layout { pack: 1, align: 4 },
    );
    let outer = structure(
        2,
        "Outer",
        vec![field("a", ResolvedType::U8), field("inner", inner)],
        Layout::default(),
    );
    let info = describe(&outer);

    assert_eq!(size_and_align(&info), (8, 4));
    assert_eq!(
        field_offsets(kind_field(&info, "fields")),
        vec![("a".into(), 0), ("inner".into(), 4)]
    );
}

#[test]
fn hidden_fields_are_described_with_their_visibility() {
    let s = structure(
        1,
        "S",
        vec![ResolvedField::new(
            ident("secret"),
            ResolvedType::U8,
            Visibility::Hidden,
        )],
        Layout::default(),
    );
    let info = describe(&s);
    let field_info = slice_item(&variant_field_type(
        &field_type(&info_type(), "kind"),
        "Struct",
        "fields",
    ));
    let secret = &elements(kind_field(&info, "fields"))[0];
    let ConstValue::Enum { variant_index, .. } = get(secret, &field_info, "visibility") else {
        panic!("visibility is an enum value");
    };
    assert_eq!(*variant_index, 0, "Hidden");
    assert_eq!(
        get(secret, &field_info, "type"),
        &ConstValue::Reflected(Reflected::TypeInfo(ResolvedType::U8))
    );
}

#[test]
fn an_enum_describes_its_tag_header_shared_fields_and_variants() {
    let str = ResolvedType::Str { mutable: false };
    let e = enumeration(
        1,
        "Shape",
        ResolvedType::U8,
        vec![field("label", str)],
        vec![field("id", ResolvedType::U16)],
        vec![
            Variant {
                name: "Circle",
                tag: NumberValue::Unsigned(3),
                header_values: vec![ConstValue::Str("circle".into())],
                fields: vec![field("radius", ResolvedType::U32)],
            },
            Variant {
                name: "Rect",
                tag: NumberValue::Unsigned(7),
                header_values: vec![ConstValue::Str("rect".into())],
                fields: vec![field("w", ResolvedType::U16), field("h", ResolvedType::U16)],
            },
        ],
    );
    let info = describe(&e);
    let view = EnumView::of(&e).unwrap();

    assert_eq!(kind_name(&info), "Enum");
    assert_eq!(
        field_offsets(kind_field(&info, "header")),
        vec![("label".into(), 1)]
    );
    assert_eq!(
        field_offsets(kind_field(&info, "shared")),
        vec![("id".into(), 13)]
    );
    let tag = kind_field(&info, "tag_field");
    let field_info = slice_item(&variant_field_type(
        &field_type(&info_type(), "kind"),
        "Struct",
        "fields",
    ));
    assert_eq!(text(get(tag, &field_info, "name")), "tag");
    assert_eq!(number(get(tag, &field_info, "offset")), 0);
    assert_eq!(
        get(tag, &field_info, "type"),
        &ConstValue::Reflected(Reflected::TypeInfo(ResolvedType::U8))
    );

    let variant_info = slice_item(&variant_field_type(
        &field_type(&info_type(), "kind"),
        "Enum",
        "variants",
    ));
    let variants = elements(kind_field(&info, "variants"));
    let rect = &variants[1];
    assert_eq!(text(get(rect, &variant_info, "name")), "Rect");
    assert_eq!(number(get(rect, &variant_info, "tag")), 7);
    assert_eq!(
        get(rect, &variant_info, "prototype"),
        &ConstValue::Reflected(Reflected::VariantPrototype {
            r#type: e.clone(),
            variant: 1
        })
    );
    let payload = layout::enum_payload_offset(&view, POINTER_BYTES) as u64;
    assert_eq!(
        field_offsets(get(rect, &variant_info, "fields")),
        vec![("w".into(), payload), ("h".into(), payload + 2)]
    );
}

#[test]
fn a_negative_variant_tag_is_truncated_to_the_tag_width() {
    let e = enumeration(
        1,
        "Signed",
        ResolvedType::I8,
        vec![],
        vec![],
        vec![Variant {
            name: "Minus",
            tag: NumberValue::Signed(-1),
            header_values: vec![],
            fields: vec![],
        }],
    );
    let info = describe(&e);
    let variant_info = slice_item(&variant_field_type(
        &field_type(&info_type(), "kind"),
        "Enum",
        "variants",
    ));
    let minus = &elements(kind_field(&info, "variants"))[0];
    assert_eq!(number(get(minus, &variant_info, "tag")), 0xFF);
}

#[test]
fn an_anonymous_enum_describes_each_member_as_a_value_field() {
    let str = ResolvedType::Str { mutable: false };
    let e = ResolvedType::AnonymousEnum {
        shape: Rc::new(ResolvedAnonymousEnum::canonicalize(vec![
            ResolvedType::U16,
            str.clone(),
        ])),
        variant: None,
    };
    let info = describe(&e);
    let view = EnumView::of(&e).unwrap();
    let payload = layout::enum_payload_offset(&view, POINTER_BYTES) as u64;

    assert_eq!(kind_name(&info), "AnonymousEnum");
    assert_eq!(text(get(&info, &info_type(), "name")), e.to_string());
    assert!(elements(get(&info, &info_type(), "path")).is_empty());
    let variant_info = slice_item(&variant_field_type(
        &field_type(&info_type(), "kind"),
        "AnonymousEnum",
        "members",
    ));
    let members = elements(kind_field(&info, "members"));
    assert_eq!(members.len(), 2);
    for (index, member) in members.iter().enumerate() {
        let ResolvedType::AnonymousEnum { shape, .. } = &e else {
            unreachable!()
        };
        assert_eq!(
            text(get(member, &variant_info, "name")),
            shape.members()[index].to_string()
        );
        assert_eq!(number(get(member, &variant_info, "tag")), index as u64);
        assert_eq!(
            field_offsets(get(member, &variant_info, "fields")),
            vec![("value".into(), payload)]
        );
    }
}

#[test]
fn a_refined_enum_is_described_by_its_parent() {
    let parent = unit_enum(1, "Light", &["Off", "On"]);
    let ResolvedType::Enum { cell, .. } = &parent else {
        unreachable!()
    };
    let refined = ResolvedType::Enum {
        cell: cell.clone(),
        variant: Some(1),
    };

    assert_eq!(type_info_ref(&refined), type_info_ref(&parent));
    assert_eq!(describe(&refined), describe(&parent));
}

#[test]
fn a_comp_argument_is_truncated_to_its_parameter_width() {
    let s = structure(1, "Signed", vec![], Layout::default());
    let ResolvedType::Struct(cell) = &s else {
        unreachable!()
    };
    cell.borrow_mut().generic_args = vec![ResolvedGenericArg::Comp(CompScalar::Int {
        r#type: CompIntType::I8,
        value: -1,
    })];
    let info = describe(&s);
    let info_type = info_type();
    let arg_type = slice_item(&field_type(&info_type, "generic_args"));
    let arg = &elements(get(&info, &info_type, "generic_args"))[0];

    assert_eq!(number(get(arg, &arg_type, "bits")), 0xFF);
    assert_eq!(
        get(arg, &arg_type, "type"),
        &ConstValue::Reflected(Reflected::TypeInfo(ResolvedType::I8))
    );
}

#[test]
fn only_a_type_table_can_be_read_under_comp() {
    let info_type = info_type();
    assert!(
        deref(
            &Reflected::TypeInfo(ResolvedType::U8),
            &info_type,
            POINTER_BYTES
        )
        .is_ok()
    );
    assert!(
        deref(
            &Reflected::VariantPrototype {
                r#type: unit_enum(1, "Light", &["Off"]),
                variant: 0
            },
            &ResolvedType::U8,
            POINTER_BYTES
        )
        .is_err()
    );
}
