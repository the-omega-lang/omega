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
