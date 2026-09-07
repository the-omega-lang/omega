use super::*;
use crate::resolved_type::ResolvedAnonymousEnum;
use std::rc::Rc;

const POINTER_BYTES: u32 = 8;

fn anonymous(members: Vec<ResolvedType>) -> ResolvedType {
    ResolvedType::AnonymousEnum {
        shape: Rc::new(ResolvedAnonymousEnum::canonicalize(members)),
        variant: None,
    }
}

fn refined(parent: &ResolvedType, index: usize) -> ResolvedType {
    match parent {
        ResolvedType::AnonymousEnum { shape, .. } => ResolvedType::AnonymousEnum {
            shape: shape.clone(),
            variant: Some(index),
        },
        other => panic!("not an anonymous enum: {other}"),
    }
}

#[test]
fn anonymous_enum_is_a_u32_tag_followed_by_the_largest_member() {
    let ty = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let view = EnumView::of(&ty).expect("an anonymous enum is enum-like");

    assert_eq!(view.tag_type, ResolvedType::U32);
    assert!(view.header.is_empty());
    assert!(view.dynamic_fields.is_empty());
    assert!(
        view.variants
            .iter()
            .all(|variant| variant.fields.len() == 1)
    );

    let tag_bytes = total_bytes(&ResolvedType::U32, POINTER_BYTES);
    assert_eq!(enum_payload_offset(&view, POINTER_BYTES), tag_bytes);
    assert_eq!(enum_payload_bytes(&view, POINTER_BYTES), 4);
    assert_eq!(total_bytes(&ty, POINTER_BYTES), tag_bytes + 4);
}

#[test]
fn anonymous_enum_payload_fits_its_largest_member() {
    // `*str` is a fat pointer, so it, not `i32`, decides the payload size.
    let ty = anonymous(vec![
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
    ]);
    let view = EnumView::of(&ty).expect("an anonymous enum is enum-like");
    let widest = total_bytes(&ResolvedType::Str { mutable: false }, POINTER_BYTES);

    assert_eq!(enum_payload_bytes(&view, POINTER_BYTES), widest);
    assert_eq!(
        total_bytes(&ty, POINTER_BYTES),
        total_bytes(&ResolvedType::U32, POINTER_BYTES) + widest
    );
}

#[test]
fn anonymous_enum_member_bodies_all_start_at_the_payload() {
    let ty = anonymous(vec![
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
        ResolvedType::Bool,
    ]);
    let view = EnumView::of(&ty).expect("an anonymous enum is enum-like");
    let payload = enum_payload_offset(&view, POINTER_BYTES);

    for index in 0..view.variants.len() {
        assert_eq!(
            enum_body_field_offset(&view, index, 0, POINTER_BYTES),
            payload
        );
    }
}

#[test]
fn anonymous_enum_tolerates_a_zero_sized_member() {
    let ty = anonymous(vec![ResolvedType::Void, ResolvedType::I32]);
    let view = EnumView::of(&ty).expect("an anonymous enum is enum-like");

    assert_eq!(view.variants.len(), 2);
    assert_eq!(enum_payload_bytes(&view, POINTER_BYTES), 4);
    assert!(!is_zero_sized(&ty));
}

#[test]
fn anonymous_enum_layout_ignores_how_the_members_were_spelled() {
    let one = anonymous(vec![
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
    ]);
    let other = anonymous(vec![
        ResolvedType::I32,
        ResolvedType::Str { mutable: false },
    ]);

    assert_eq!(
        leaves_of(&one, POINTER_BYTES),
        leaves_of(&other, POINTER_BYTES)
    );
    assert_eq!(type_alignment(&one), type_alignment(&other));

    let one_view = EnumView::of(&one).expect("an anonymous enum is enum-like");
    let other_view = EnumView::of(&other).expect("an anonymous enum is enum-like");
    for index in 0..one_view.variants.len() {
        assert_eq!(
            one_view.variants[index].fields,
            other_view.variants[index].fields
        );
        assert_eq!(
            enum_body_field_offset(&one_view, index, 0, POINTER_BYTES),
            enum_body_field_offset(&other_view, index, 0, POINTER_BYTES)
        );
    }
}

#[test]
fn refinement_never_changes_an_anonymous_enum_value() {
    // Refinement is a proof about the current value, so a refined binding
    // must stay byte-identical to the parent -- that is what makes widening
    // back a plain copy.
    let parent = anonymous(vec![
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
    ]);
    for index in 0..2 {
        let refined = refined(&parent, index);
        assert_eq!(
            leaves_of(&refined, POINTER_BYTES),
            leaves_of(&parent, POINTER_BYTES)
        );
        assert_eq!(
            total_bytes(&refined, POINTER_BYTES),
            total_bytes(&parent, POINTER_BYTES)
        );
        assert_eq!(type_alignment(&refined), type_alignment(&parent));
    }
}

#[test]
fn a_nested_anonymous_member_lays_out_as_its_flattened_leaves() {
    let inner = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let nested = anonymous(vec![inner.clone(), ResolvedType::Bool]);
    let view = EnumView::of(&nested).expect("an anonymous enum is enum-like");

    assert_eq!(view.variants.len(), 2);
    assert_eq!(
        leaves_of(&nested, POINTER_BYTES),
        leaves_of(&inner, POINTER_BYTES)
    );
}

#[test]
fn a_member_merely_containing_an_anonymous_enum_lays_out_as_one_member() {
    let inner = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let wrapper = ResolvedType::SizedArray(Box::new(inner.clone()), 1);
    let outer = anonymous(vec![wrapper.clone(), ResolvedType::Bool]);
    let view = EnumView::of(&outer).expect("an anonymous enum is enum-like");

    assert_eq!(view.variants.len(), 2);
    assert_eq!(
        enum_payload_bytes(&view, POINTER_BYTES),
        total_bytes(&wrapper, POINTER_BYTES)
    );
}

#[test]
fn a_function_value_is_one_pointer_width_code_pointer() {
    let function = ResolvedType::Function(crate::resolved_type::ResolvedFunctionType {
        params: Vec::new(),
        return_type: Box::new(ResolvedType::Void),
        is_variadic: false,
        self_mode: None,
        calling_convention: crate::resolved_type::CallingConvention::Omega,
    });
    let data_pointer = ResolvedType::Pointer {
        pointee: Box::new(ResolvedType::I32),
        mutable: false,
    };

    for pointer_bytes in [2, 4, 8] {
        assert_eq!(leaves_of(&function, pointer_bytes), vec![Leaf::FnPtr]);
        assert_eq!(leaves_of(&data_pointer, pointer_bytes), vec![Leaf::Ptr]);
        assert_eq!(Leaf::FnPtr.bytes(pointer_bytes), pointer_bytes);
        assert_eq!(
            total_bytes(&function, pointer_bytes),
            total_bytes(&data_pointer, pointer_bytes)
        );
    }
}

use crate::annotations::Layout;
use crate::resolved_type::{
    ResolvedEnumType, ResolvedEnumVariant, ResolvedField, ResolvedStructType, ResolvedUnionType,
};
use omega_hir::ids::{HirId, ModuleId};
use omega_parser::prelude::{Ident, Visibility};
use std::cell::RefCell;

fn hir_id(n: u32) -> HirId {
    HirId {
        module: ModuleId(0),
        local: n,
    }
}

fn field(name: &str, r#type: ResolvedType) -> ResolvedField {
    ResolvedField::new(Ident(name.into()), r#type, Visibility::Exposed)
}

fn structure(fields: Vec<ResolvedField>, layout: Layout) -> ResolvedType {
    ResolvedType::Struct(Rc::new(RefCell::new(ResolvedStructType {
        id: hir_id(1),
        name: Ident("S".into()),
        module_path: vec![],
        generic_args: vec![],
        fields,
        functions: vec![],
        layout,
        suppress: vec![],
        is_marker: false,
    })))
}

fn union(fields: Vec<ResolvedField>) -> ResolvedType {
    ResolvedType::Union(Rc::new(RefCell::new(ResolvedUnionType {
        id: hir_id(2),
        name: Ident("U".into()),
        module_path: vec![],
        generic_args: vec![],
        fields,
        functions: vec![],
        suppress: vec![],
    })))
}

fn named_enum(
    header: Vec<ResolvedField>,
    dynamic_fields: Vec<ResolvedField>,
    variants: Vec<Vec<ResolvedField>>,
    layout: Layout,
) -> ResolvedType {
    ResolvedType::Enum {
        cell: Rc::new(RefCell::new(ResolvedEnumType {
            id: hir_id(3),
            name: Ident("E".into()),
            module_path: vec![],
            generic_args: vec![],
            tag_type: ResolvedType::U32,
            header,
            dynamic_fields,
            variants: variants
                .into_iter()
                .enumerate()
                .map(|(index, fields)| ResolvedEnumVariant {
                    name: Ident(format!("V{index}").into()),
                    tag: NumberValue::Unsigned(index as u64),
                    header_values: vec![],
                    fields,
                })
                .collect(),
            functions: vec![],
            layout,
            suppress: vec![],
        })),
        variant: None,
    }
}

fn aligned(align: u32) -> Layout {
    Layout { pack: 1, align }
}

/// The canonical shape from the alignment issue: a container inherits the
/// address requirement of anything stored inline in it.
#[test]
fn a_struct_inherits_its_fields_alignment() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let outer = structure(
        vec![
            field("pad", ResolvedType::U8),
            field("inner", inner.clone()),
        ],
        Layout::default(),
    );

    assert_eq!(type_alignment(&inner), 16);
    assert_eq!(type_alignment(&outer), 16);

    let ResolvedType::Struct(cell) = &outer else {
        unreachable!()
    };
    let layout = struct_layout(&cell.borrow(), POINTER_BYTES);
    assert_eq!(layout.byte_offsets, vec![0, 16]);
    assert_eq!(layout.packed_end, 32);
    assert_eq!(total_bytes(&outer, POINTER_BYTES), 32);
    assert_eq!(
        leaves_of(&outer, POINTER_BYTES).len(),
        1 + 15 + 1 + 8 // pad, interior padding, the i64 leaf, trailing padding
    );
}

#[test]
fn a_weaker_container_annotation_never_lowers_a_member_requirement() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let outer = structure(vec![field("inner", inner)], aligned(1));

    assert_eq!(type_alignment(&outer), 16);
}

#[test]
fn pack_places_fields_without_weakening_alignment() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let outer = ResolvedType::Struct(Rc::new(RefCell::new(ResolvedStructType {
        id: hir_id(1),
        name: Ident("S".into()),
        module_path: vec![],
        generic_args: vec![],
        fields: vec![field("pad", ResolvedType::U8), field("inner", inner)],
        functions: vec![],
        layout: Layout { pack: 4, align: 1 },
        suppress: vec![],
        is_marker: false,
    })));

    assert_eq!(type_alignment(&outer), 16);
    assert_eq!(field_byte_offset_of(&outer, 1), 16);
}

fn field_byte_offset_of(ty: &ResolvedType, index: usize) -> u32 {
    let ResolvedType::Struct(cell) = ty else {
        panic!("not a struct")
    };
    field_byte_offset(&cell.borrow(), index, POINTER_BYTES)
}

#[test]
fn size_rounds_up_to_effective_alignment_so_array_stride_stays_aligned() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let outer = structure(
        vec![field("inner", inner), field("tail", ResolvedType::U8)],
        Layout::default(),
    );

    // 16 bytes of `inner` plus a trailing byte, rounded back up to 16.
    assert_eq!(total_bytes(&outer, POINTER_BYTES), 32);

    let array = ResolvedType::SizedArray(Box::new(outer.clone()), 3);
    assert_eq!(type_alignment(&array), 16);
    assert_eq!(total_bytes(&array, POINTER_BYTES), 96);
}

#[test]
fn a_zero_length_array_keeps_its_element_alignment_without_bytes() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let array = ResolvedType::SizedArray(Box::new(inner), 0);

    assert_eq!(type_alignment(&array), 16);
    assert_eq!(total_bytes(&array, POINTER_BYTES), 0);
}

#[test]
fn an_empty_aligned_value_stays_zero_sized() {
    let empty = structure(vec![], aligned(64));

    assert_eq!(type_alignment(&empty), 64);
    assert_eq!(total_bytes(&empty, POINTER_BYTES), 0);
    assert!(is_zero_sized(&empty));
}

#[test]
fn a_union_takes_the_maximum_member_alignment_and_rounds_its_size() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let u = union(vec![
        field("a", ResolvedType::U8),
        field("b", inner),
        field("c", ResolvedType::I32),
    ]);

    assert_eq!(type_alignment(&u), 16);
    assert_eq!(total_bytes(&u, POINTER_BYTES), 16);

    let wide = union(vec![
        field("a", ResolvedType::I64),
        field(
            "b",
            structure(vec![field("v", ResolvedType::U8)], aligned(16)),
        ),
    ]);
    assert_eq!(total_bytes(&wide, POINTER_BYTES), 16);
}

#[test]
fn a_plain_union_keeps_alignment_one() {
    let u = union(vec![
        field("a", ResolvedType::I64),
        field("b", ResolvedType::U8),
    ]);

    assert_eq!(type_alignment(&u), 1);
    assert_eq!(total_bytes(&u, POINTER_BYTES), 8);
}

#[test]
fn a_named_enum_inherits_prefix_and_payload_alignment() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(32));

    let from_header = named_enum(
        vec![field("h", inner.clone())],
        vec![],
        vec![vec![field("x", ResolvedType::U8)]],
        Layout::default(),
    );
    assert_eq!(type_alignment(&from_header), 32);
    assert_eq!(total_bytes(&from_header, POINTER_BYTES) % 32, 0);

    let from_dynamic = named_enum(
        vec![],
        vec![field("d", inner.clone())],
        vec![vec![field("x", ResolvedType::U8)]],
        Layout::default(),
    );
    assert_eq!(type_alignment(&from_dynamic), 32);

    let from_body = named_enum(
        vec![],
        vec![],
        vec![vec![field("x", ResolvedType::U8)], vec![field("y", inner)]],
        Layout::default(),
    );
    assert_eq!(type_alignment(&from_body), 32);
    let view = EnumView::of(&from_body).expect("an enum is enum-like");
    assert_eq!(enum_payload_offset(&view, POINTER_BYTES) % 32, 0);
}

#[test]
fn a_named_enum_keeps_its_declared_alignment_when_no_member_needs_more() {
    let e = named_enum(
        vec![],
        vec![],
        vec![vec![field("x", ResolvedType::U8)]],
        aligned(8),
    );

    assert_eq!(type_alignment(&e), 8);
    assert_eq!(total_bytes(&e, POINTER_BYTES), 8);
}

#[test]
fn an_anonymous_enum_inherits_its_members_alignment() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let ty = anonymous(vec![inner, ResolvedType::Bool]);

    assert_eq!(type_alignment(&ty), 16);
    assert_eq!(total_bytes(&ty, POINTER_BYTES) % 16, 0);

    for index in 0..2 {
        assert_eq!(type_alignment(&refined(&ty, index)), 16);
        assert_eq!(
            leaves_of(&refined(&ty, index), POINTER_BYTES),
            leaves_of(&ty, POINTER_BYTES)
        );
    }
}

#[test]
fn alignment_does_not_follow_a_pointer_to_an_aligned_type() {
    let inner = structure(vec![field("v", ResolvedType::I64)], aligned(16));
    let pointer = ResolvedType::Pointer {
        pointee: Box::new(inner.clone()),
        mutable: false,
    };
    let slice = ResolvedType::Slice {
        item: Box::new(inner),
        mutable: false,
    };

    assert_eq!(type_alignment(&pointer), 1);
    assert_eq!(type_alignment(&slice), 1);
    assert_eq!(
        type_alignment(&structure(vec![field("p", pointer)], Layout::default())),
        1
    );
}

#[test]
fn packed_primitive_only_layouts_are_unchanged() {
    for pointer_bytes in [2, 4, 8] {
        let plain = structure(
            vec![
                field("a", ResolvedType::U8),
                field("b", ResolvedType::I64),
                field("c", ResolvedType::USize),
            ],
            Layout::default(),
        );

        assert_eq!(type_alignment(&plain), 1);
        assert_eq!(
            total_bytes(&plain, pointer_bytes),
            1 + 8 + pointer_bytes,
            "pointer width {pointer_bytes}"
        );
        assert_eq!(field_byte_offset_of(&plain, 2), 9);
    }
}

#[test]
fn effective_alignment_is_independent_of_pointer_width() {
    let inner = structure(vec![field("v", ResolvedType::USize)], aligned(8));
    let outer = structure(
        vec![field("lead", ResolvedType::U8), field("inner", inner)],
        Layout::default(),
    );

    for pointer_bytes in [2, 4, 8] {
        assert_eq!(type_alignment(&outer), 8);
        assert_eq!(field_byte_offset_of(&outer, 1), 8);
        assert_eq!(total_bytes(&outer, pointer_bytes), 16);
    }
}
