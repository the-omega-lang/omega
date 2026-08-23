use super::*;

fn fn_type(convention: CallingConvention) -> ResolvedFunctionType {
    ResolvedFunctionType {
        params: vec![(Ident("x".into()), ResolvedType::I32)],
        return_type: Box::new(ResolvedType::I32),
        is_variadic: false,
        self_mode: None,
        calling_convention: convention,
    }
}

#[test]
fn calling_convention_variants_are_pairwise_distinct() {
    assert_ne!(CallingConvention::Omega, CallingConvention::C);
    assert_ne!(CallingConvention::Omega, CallingConvention::SysV64);
    assert_ne!(CallingConvention::C, CallingConvention::SysV64);
}

#[test]
fn function_types_differing_only_in_calling_convention_are_unequal() {
    // `foreign(c) (i32) => i32` and `(i32) => i32` must stay distinct types
    // even on a target where both currently lower to the same machine
    // convention -- see docs/language/foreign-function-interface.md.
    let omega = fn_type(CallingConvention::Omega);
    let c = fn_type(CallingConvention::C);
    let sysv64 = fn_type(CallingConvention::SysV64);
    assert_ne!(omega, c);
    assert_ne!(omega, sysv64);
    assert_ne!(c, sysv64);
}

#[test]
fn function_pointer_assignment_rejects_calling_convention_mismatch() {
    let expected = ResolvedType::Function(fn_type(CallingConvention::Omega));
    let found = ResolvedType::Function(fn_type(CallingConvention::C));
    assert!(!expected.accepts(&found));
    assert!(expected.accepts(&ResolvedType::Function(fn_type(CallingConvention::Omega))));
}

fn spec_cell(id: u32, name: &str) -> Rc<RefCell<ResolvedSpecType>> {
    Rc::new(RefCell::new(ResolvedSpecType {
        id: HirId {
            module: omega_hir::ModuleId(0),
            local: id,
        },
        name: Ident(name.to_string()),
        visibility: Visibility::Exposed,
        generics: vec![],
        module_path: vec![],
        type_args: vec![],
        is_object_safe: true,
        functions: vec![],
        suppress: vec![],
    }))
}

#[test]
fn spec_shape_canonicalizes_reordered_members_identically() {
    let a = spec_cell(1, "A");
    let b = spec_cell(2, "B");
    let ab = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(a.clone(), vec![]),
        ResolvedSpecApplication::new(b.clone(), vec![]),
    ]);
    let ba = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(b, vec![]),
        ResolvedSpecApplication::new(a, vec![]),
    ]);
    assert_eq!(ab, ba);
    assert_eq!(ab.to_string(), "A + B");
}

#[test]
fn spec_shape_canonicalizes_duplicate_members_away() {
    let a1 = spec_cell(1, "A");
    let a2 = spec_cell(1, "A");
    let shape = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(a1, vec![]),
        ResolvedSpecApplication::new(a2, vec![]),
    ]);
    assert_eq!(shape.members.len(), 1);
}

fn struct_cell(id: u32, name: &str, type_args: Vec<ResolvedType>) -> ResolvedType {
    ResolvedType::Struct(Rc::new(RefCell::new(ResolvedStructType {
        id: HirId {
            module: omega_hir::ModuleId(0),
            local: id,
        },
        name: Ident(name.to_string()),
        module_path: vec![Ident("pkg".into())],
        type_args,
        fields: vec![],
        functions: vec![],
        layout: crate::annotations::Layout::default(),
        suppress: vec![],
        is_marker: false,
    })))
}

fn anonymous(members: Vec<ResolvedType>) -> ResolvedType {
    ResolvedType::AnonymousEnum {
        shape: Rc::new(ResolvedAnonymousEnum::canonicalize(members)),
        variant: None,
    }
}

fn shape_of(ty: &ResolvedType) -> Rc<ResolvedAnonymousEnum> {
    match ty {
        ResolvedType::AnonymousEnum { shape, .. } => shape.clone(),
        other => panic!("not an anonymous enum: {other}"),
    }
}

fn hash_of(ty: &ResolvedType) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    ty.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn anonymous_enum_reordered_spellings_are_one_type() {
    let ab = anonymous(vec![ResolvedType::I32, ResolvedType::Str { mutable: false }]);
    let ba = anonymous(vec![ResolvedType::Str { mutable: false }, ResolvedType::I32]);
    assert_eq!(ab, ba);
    assert_eq!(hash_of(&ab), hash_of(&ba));
    assert_eq!(
        shape_of(&ab).members().to_vec(),
        shape_of(&ba).members().to_vec()
    );
}

#[test]
fn anonymous_enum_collapses_exact_duplicates() {
    let duplicated = anonymous(vec![ResolvedType::I32, ResolvedType::I32]);
    let single = anonymous(vec![ResolvedType::I32]);
    assert_eq!(duplicated, single);
    assert_eq!(shape_of(&duplicated).members().len(), 1);
}

#[test]
fn anonymous_enum_member_indices_are_the_tags() {
    let shape = shape_of(&anonymous(vec![
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
        ResolvedType::Bool,
    ]));
    for (index, member) in shape.members().iter().enumerate() {
        assert_eq!(shape.index_of(member), Some(index));
    }
    // The same members spelled in any other order agree on every index.
    let other = shape_of(&anonymous(vec![
        ResolvedType::Bool,
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
    ]));
    assert_eq!(shape.members().to_vec(), other.members().to_vec());
}

#[test]
fn anonymous_enum_does_not_flatten_a_nested_member() {
    let inner = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let nested = anonymous(vec![inner.clone(), ResolvedType::Char]);
    let flat = anonymous(vec![ResolvedType::I32, ResolvedType::Bool, ResolvedType::Char]);
    assert_ne!(nested, flat);
    assert_eq!(shape_of(&nested).members().len(), 2);
    assert!(shape_of(&nested).members().contains(&inner));
}

#[test]
fn anonymous_enum_refinement_widens_but_never_converts_between_shapes() {
    let parent = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let shape = shape_of(&parent);
    let refined = ResolvedType::AnonymousEnum {
        shape: shape.clone(),
        variant: Some(0),
    };
    assert_ne!(parent, refined);
    assert!(parent.accepts(&refined));
    assert!(!refined.accepts(&parent));
    assert_eq!(refined.widened(), parent);
    assert_eq!(refined.lookup_key(), parent);
    assert_eq!(
        refined.refined_anonymous_member().map(|(i, _)| i),
        Some(0usize)
    );

    // A member is never already the enum, and a subset is never the superset:
    // both need a real construction.
    let superset = anonymous(vec![ResolvedType::I32, ResolvedType::Bool, ResolvedType::Char]);
    assert!(!parent.accepts(&ResolvedType::I32));
    assert!(!superset.accepts(&parent));
    assert!(!parent.accepts(&superset));
}

#[test]
fn anonymous_enum_tag_domain_is_the_u16_range() {
    let members: Vec<ResolvedType> = (0..=ResolvedAnonymousEnum::MAX_MEMBERS as u32)
        .map(|size| ResolvedType::SizedArray(Box::new(ResolvedType::U8), size))
        .collect();
    let over = ResolvedAnonymousEnum::canonicalize(members.clone());
    assert_eq!(over.members().len(), ResolvedAnonymousEnum::MAX_MEMBERS + 1);
    assert!(over.exceeds_tag_domain());

    let exact = ResolvedAnonymousEnum::canonicalize(
        members[..ResolvedAnonymousEnum::MAX_MEMBERS].to_vec(),
    );
    assert!(!exact.exceeds_tag_domain());
}

#[test]
fn structural_key_separates_nominal_types_display_renders_alike() {
    // `Display` prints a bare name with no generic arguments, so it cannot
    // order these two apart; the canonical key must.
    let int_pair = struct_cell(1, "Pair", vec![ResolvedType::I32]);
    let float_pair = struct_cell(2, "Pair", vec![ResolvedType::F64]);
    assert_eq!(int_pair.to_string(), float_pair.to_string());
    assert_ne!(
        crate::type_key::structural_key(&int_pair),
        crate::type_key::structural_key(&float_pair)
    );

    // ...and an anonymous enum over both keeps them as two distinct members
    // in one deterministic order, whichever way it was spelled.
    let one = anonymous(vec![int_pair.clone(), float_pair.clone()]);
    let other = anonymous(vec![float_pair, int_pair]);
    assert_eq!(one, other);
    assert_eq!(shape_of(&one).members().len(), 2);
}

#[test]
fn spec_shape_orders_generic_applications_of_one_spec_deterministically() {
    // The old ordering key rendered arguments with `Display`, so
    // `Convert<Pair<i32>>` and `Convert<Pair<f64>>` collided and their
    // relative order fell back to source order.
    let convert = spec_cell(1, "Convert");
    let int_pair = struct_cell(2, "Pair", vec![ResolvedType::I32]);
    let float_pair = struct_cell(3, "Pair", vec![ResolvedType::F64]);
    let forwards = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(convert.clone(), vec![int_pair.clone()]),
        ResolvedSpecApplication::new(convert.clone(), vec![float_pair.clone()]),
    ]);
    let backwards = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(convert.clone(), vec![float_pair]),
        ResolvedSpecApplication::new(convert, vec![int_pair]),
    ]);
    assert_eq!(forwards.members.len(), 2);
    assert_eq!(forwards, backwards);
}
