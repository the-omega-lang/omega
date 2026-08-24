use super::*;

fn fn_type(convention: CallingConvention) -> ResolvedFunctionType {
    ResolvedFunctionType {
        params: vec![ResolvedFunctionParam::described(
            Ident("x".into()),
            ResolvedType::I32,
        )],
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

#[test]
fn function_display_distinguishes_calling_conventions() {
    // A rejected cast between these two must not render both sides as the
    // same text, or the diagnostic is unactionable.
    assert_eq!(
        ResolvedType::Function(fn_type(CallingConvention::Omega)).to_string(),
        "(x: i32) => i32"
    );
    assert_eq!(
        ResolvedType::Function(fn_type(CallingConvention::C)).to_string(),
        "foreign(c) (x: i32) => i32"
    );
    assert_eq!(
        ResolvedType::Function(fn_type(CallingConvention::SysV64)).to_string(),
        "foreign(sysv64) (x: i32) => i32"
    );
}

#[test]
fn function_display_distinguishes_self_modes() {
    let with_self = |self_mode| {
        let mut fn_type = fn_type(CallingConvention::Omega);
        fn_type.self_mode = Some(self_mode);
        ResolvedType::Function(fn_type).to_string()
    };
    assert_eq!(with_self(SelfMode::Value), "(self, x: i32) => i32");
    assert_eq!(with_self(SelfMode::MutValue), "(mut self, x: i32) => i32");
    assert_eq!(with_self(SelfMode::Pointer), "(*self, x: i32) => i32");
    assert_eq!(
        with_self(SelfMode::MutPointer),
        "(*mut self, x: i32) => i32"
    );
}

#[test]
fn function_display_keeps_a_variadic_tail_after_self() {
    let mut variadic = fn_type(CallingConvention::C);
    variadic.is_variadic = true;
    assert_eq!(
        ResolvedType::Function(variadic.clone()).to_string(),
        "foreign(c) (x: i32, ...) => i32"
    );
    let mut no_params = variadic;
    no_params.params.clear();
    assert_eq!(
        ResolvedType::Function(no_params).to_string(),
        "foreign(c) (...) => i32"
    );
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

fn enum_cell(id: u32, name: &str) -> ResolvedType {
    ResolvedType::Enum {
        cell: Rc::new(RefCell::new(ResolvedEnumType {
            id: HirId {
                module: omega_hir::ModuleId(0),
                local: id,
            },
            name: Ident(name.to_string()),
            module_path: vec![Ident("pkg".into())],
            type_args: vec![],
            tag_type: ResolvedType::U8,
            header: vec![],
            dynamic_fields: vec![],
            variants: vec![],
            functions: vec![],
            layout: crate::annotations::Layout::default(),
            suppress: vec![],
        })),
        variant: None,
    }
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
    let ab = anonymous(vec![
        ResolvedType::I32,
        ResolvedType::Str { mutable: false },
    ]);
    let ba = anonymous(vec![
        ResolvedType::Str { mutable: false },
        ResolvedType::I32,
    ]);
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
fn anonymous_enum_flattens_nested_members_recursively() {
    let inner = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let middle = anonymous(vec![inner, ResolvedType::Char]);
    let nested = anonymous(vec![middle, ResolvedType::U8]);
    let flat = anonymous(vec![
        ResolvedType::I32,
        ResolvedType::Bool,
        ResolvedType::Char,
        ResolvedType::U8,
    ]);
    assert_eq!(nested, flat);
    assert_eq!(hash_of(&nested), hash_of(&flat));
    assert_eq!(shape_of(&nested).members().len(), 4);
    assert!(
        !shape_of(&nested)
            .members()
            .iter()
            .any(|member| matches!(member, ResolvedType::AnonymousEnum { .. }))
    );
}

#[test]
fn anonymous_enum_flattening_collapses_duplicates_across_nesting() {
    let left = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let right = anonymous(vec![ResolvedType::Bool, ResolvedType::Char]);
    let merged = anonymous(vec![left, right, ResolvedType::I32]);
    assert_eq!(
        merged,
        anonymous(vec![
            ResolvedType::I32,
            ResolvedType::Bool,
            ResolvedType::Char
        ])
    );
    assert_eq!(shape_of(&merged).members().len(), 3);
}

#[test]
fn anonymous_enum_flattening_ignores_a_members_refinement() {
    let inner = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let refined = ResolvedType::AnonymousEnum {
        shape: shape_of(&inner),
        variant: Some(0),
    };
    assert_eq!(
        anonymous(vec![refined, ResolvedType::Char]),
        anonymous(vec![
            ResolvedType::I32,
            ResolvedType::Bool,
            ResolvedType::Char
        ])
    );
}

#[test]
fn anonymous_enum_flattening_stops_at_every_other_constructor() {
    let inner = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let named = enum_cell(7, "Named");

    // A named enum is nominal, so it is one member whatever its variants are.
    let with_named = anonymous(vec![named.clone(), ResolvedType::Char]);
    assert_eq!(shape_of(&with_named).members().len(), 2);
    assert!(shape_of(&with_named).members().contains(&named));

    // A type that merely *contains* an anonymous enum is still one member.
    let pointer = ResolvedType::Pointer {
        pointee: Box::new(inner.clone()),
        mutable: false,
    };
    let array = ResolvedType::SizedArray(Box::new(inner.clone()), 4);
    let container = struct_cell(9, "Box", vec![inner]);
    let boundaries = anonymous(vec![pointer.clone(), array.clone(), container.clone()]);
    assert_eq!(shape_of(&boundaries).members().len(), 3);
    for member in [pointer, array, container] {
        assert!(shape_of(&boundaries).members().contains(&member));
    }
}

#[test]
fn anonymous_enum_tags_follow_the_flattened_order() {
    let nested = anonymous(vec![
        anonymous(vec![
            ResolvedType::Str { mutable: false },
            ResolvedType::I32,
        ]),
        ResolvedType::Bool,
    ]);
    let flat = anonymous(vec![
        ResolvedType::Bool,
        ResolvedType::I32,
        ResolvedType::Str { mutable: false },
    ]);
    let shape = shape_of(&nested);
    assert_eq!(shape.members().to_vec(), shape_of(&flat).members().to_vec());
    for (index, member) in shape.members().iter().enumerate() {
        assert_eq!(shape.index_of(member), Some(index));
    }
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
    let superset = anonymous(vec![
        ResolvedType::I32,
        ResolvedType::Bool,
        ResolvedType::Char,
    ]);
    assert!(!parent.accepts(&ResolvedType::I32));
    assert!(!superset.accepts(&parent));
    assert!(!parent.accepts(&superset));
}

#[test]
fn anonymous_enum_subset_remap_retags_every_member() {
    // `i32` sorts before both source members, so every source tag shifts:
    // a widening implementation that kept the source tag would land on the
    // wrong member here.
    let source = shape_of(&anonymous(vec![ResolvedType::Bool, ResolvedType::Char]));
    let destination = shape_of(&anonymous(vec![
        ResolvedType::Char,
        ResolvedType::I32,
        ResolvedType::Bool,
    ]));
    assert_eq!(destination.subset_remap(&source), Some(vec![1, 2]));

    // A reordered spelling is the same type, so it remaps identically.
    let reordered = shape_of(&anonymous(vec![ResolvedType::Char, ResolvedType::Bool]));
    assert_eq!(destination.subset_remap(&reordered), Some(vec![1, 2]));
}

#[test]
fn anonymous_enum_subset_remap_of_an_equal_shape_is_the_identity() {
    let shape = shape_of(&anonymous(vec![ResolvedType::I32, ResolvedType::Bool]));
    let same = shape_of(&anonymous(vec![ResolvedType::Bool, ResolvedType::I32]));
    assert_eq!(shape.subset_remap(&same), Some(vec![0, 1]));
}

#[test]
fn anonymous_enum_subset_remap_rejects_anything_but_a_subset() {
    let narrow = shape_of(&anonymous(vec![ResolvedType::I32, ResolvedType::Bool]));
    let wide = shape_of(&anonymous(vec![
        ResolvedType::I32,
        ResolvedType::Bool,
        ResolvedType::Char,
    ]));
    assert_eq!(narrow.subset_remap(&wide), None);

    let overlapping = shape_of(&anonymous(vec![ResolvedType::I32, ResolvedType::Char]));
    assert_eq!(narrow.subset_remap(&overlapping), None);
    assert_eq!(overlapping.subset_remap(&narrow), None);
}

#[test]
fn anonymous_enum_tag_domain_is_the_u16_range() {
    let members: Vec<ResolvedType> = (0..=ResolvedAnonymousEnum::MAX_MEMBERS as u32)
        .map(|size| ResolvedType::SizedArray(Box::new(ResolvedType::U8), size))
        .collect();
    let over = ResolvedAnonymousEnum::canonicalize(members.clone());
    assert_eq!(over.members().len(), ResolvedAnonymousEnum::MAX_MEMBERS + 1);
    assert!(over.exceeds_tag_domain());

    let exact =
        ResolvedAnonymousEnum::canonicalize(members[..ResolvedAnonymousEnum::MAX_MEMBERS].to_vec());
    assert!(!exact.exceeds_tag_domain());

    // The limit applies to the flattened list, so two shapes that each fit can
    // combine into one that does not.
    let split = ResolvedAnonymousEnum::MAX_MEMBERS / 2;
    let left = anonymous(members[..split].to_vec());
    let right = anonymous(members[split..].to_vec());
    let combined = ResolvedAnonymousEnum::canonicalize(vec![left, right]);
    assert_eq!(
        combined.members().len(),
        ResolvedAnonymousEnum::MAX_MEMBERS + 1
    );
    assert!(combined.exceeds_tag_domain());
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

fn method(self_mode: Option<SelfMode>, receiver: ResolvedType) -> ResolvedMethod {
    let mut fn_type = fn_type(CallingConvention::Omega);
    if self_mode.is_some() {
        fn_type.params.insert(
            0,
            ResolvedFunctionParam::described(Ident("self".into()), receiver.clone()),
        );
    }
    fn_type.self_mode = self_mode;
    ResolvedMethod {
        decl_id: HirId {
            module: omega_hir::ModuleId(0),
            local: 0,
        },
        fn_type,
        visibility: Visibility::Hidden,
        annotations: crate::annotations::ResolvedAnnotations::default(),
        source: None,
    }
}

fn owner_pointer() -> ResolvedType {
    ResolvedType::Pointer {
        pointee: Box::new(ResolvedType::I32),
        mutable: false,
    }
}

#[test]
fn namespace_classifies_by_receiver_presence() {
    assert_eq!(
        method(None, owner_pointer()).namespace(),
        FunctionNamespace::Static
    );
    for mode in [
        SelfMode::Value,
        SelfMode::MutValue,
        SelfMode::Pointer,
        SelfMode::MutPointer,
    ] {
        assert_eq!(
            method(Some(mode), owner_pointer()).namespace(),
            FunctionNamespace::Member
        );
    }
}

#[test]
fn member_value_view_keeps_receiver_and_drops_declaration_metadata() {
    let member = method(Some(SelfMode::Pointer), owner_pointer());
    let value = member.value_fn_type();

    assert_eq!(value.self_mode, None);
    assert_eq!(value.params.len(), member.fn_type.params.len());
    assert_eq!(value.params[0].r#type, owner_pointer());
    assert_eq!(value.params[0].name, None);
    assert_eq!(
        ResolvedType::Function(value).to_string(),
        "(*i32, x: i32) => i32"
    );
}

#[test]
fn static_value_view_is_unchanged() {
    let r#static = method(None, owner_pointer());
    assert_eq!(r#static.value_fn_type(), r#static.fn_type);
}

#[test]
fn unbound_member_value_stores_into_named_and_unnamed_receiver_types() {
    // The unbound member value's receiver carries no descriptor, and a
    // written function type may describe that parameter or not. Descriptors
    // are not identity, so all three spellings are one type.
    let value =
        ResolvedType::Function(method(Some(SelfMode::Pointer), owner_pointer()).value_fn_type());

    let mut described = fn_type(CallingConvention::Omega);
    described.params.insert(
        0,
        ResolvedFunctionParam::described(Ident("target".into()), owner_pointer()),
    );
    let described = ResolvedType::Function(described);

    let mut bare = fn_type(CallingConvention::Omega);
    bare.params
        .insert(0, ResolvedFunctionParam::anonymous(owner_pointer()));
    let bare = ResolvedType::Function(bare);

    for written in [&described, &bare] {
        assert!(written.accepts(&value));
        assert!(value.accepts(written));
    }
    assert_eq!(described, bare);
}

/// The same `(i32) => i32` written three ways: undescribed, and described
/// with two different names.
fn described_as(name: Option<&str>) -> ResolvedFunctionType {
    let mut described = fn_type(CallingConvention::Omega);
    described.params[0].name = name.map(|name| Ident(name.into()));
    described
}

#[test]
fn parameter_descriptors_are_not_part_of_function_type_identity() {
    let a = described_as(Some("a"));
    let b = described_as(Some("b"));
    let bare = described_as(None);

    assert_eq!(a, b);
    assert_eq!(a, bare);
    assert!(ResolvedType::Function(a.clone()).accepts(&ResolvedType::Function(bare.clone())));
    assert!(ResolvedType::Function(bare.clone()).accepts(&ResolvedType::Function(b.clone())));

    let spellings: std::collections::HashSet<ResolvedType> = [a, b, bare]
        .into_iter()
        .map(ResolvedType::Function)
        .collect();
    assert_eq!(spellings.len(), 1);
}

#[test]
fn structural_key_ignores_parameter_descriptors() {
    let key = |fn_type| crate::type_key::structural_key(&ResolvedType::Function(fn_type));
    assert_eq!(key(described_as(Some("a"))), key(described_as(Some("b"))));
    assert_eq!(key(described_as(Some("a"))), key(described_as(None)));
}

#[test]
fn everything_except_descriptors_still_separates_function_types() {
    let base = described_as(Some("a"));

    let mut other_param_type = described_as(Some("a"));
    other_param_type.params[0].r#type = ResolvedType::I64;

    let mut other_return_type = described_as(Some("b"));
    other_return_type.return_type = Box::new(ResolvedType::Void);

    let mut extra_param = described_as(None);
    extra_param
        .params
        .push(ResolvedFunctionParam::anonymous(ResolvedType::I32));

    let mut variadic = described_as(None);
    variadic.calling_convention = CallingConvention::C;
    variadic.is_variadic = true;

    let mut member = described_as(None);
    member.self_mode = Some(SelfMode::Pointer);

    for different in [
        other_param_type,
        other_return_type,
        extra_param,
        variadic,
        member,
        fn_type(CallingConvention::C),
    ] {
        assert_ne!(base, different);
        assert!(!ResolvedType::Function(base.clone()).accepts(&ResolvedType::Function(different)));
    }
}

#[test]
fn namespace_selection_separates_identical_signatures() {
    let name = Ident("same".into());
    let functions = vec![
        (name.clone(), method(None, owner_pointer())),
        (
            name.clone(),
            method(Some(SelfMode::Pointer), owner_pointer()),
        ),
    ];

    assert_eq!(FunctionNamespace::Static.select(&functions, &name).len(), 1);
    assert_eq!(FunctionNamespace::Member.select(&functions, &name).len(), 1);
    assert_eq!(
        FunctionNamespace::Static.select(&functions, &name)[0].namespace(),
        FunctionNamespace::Static
    );
    assert_eq!(
        FunctionNamespace::Member.spelling("Thing", &name),
        "Thing::self::same"
    );
    assert_eq!(
        FunctionNamespace::Static.spelling("Thing", &name),
        "Thing::same"
    );
}
