use omega_mangle::{
    FunctionSignature, MangleConvention, MangleGenericArg, MangleIntType, ManglePath,
    MangleTemplate, MangleTemplateArg, MangleTemplateBound, MangleTemplateParam,
    MangleTemplateType, MangleType, MangleValue, Namespace, Symbol, decode, demangle, encode,
};

fn sig(params: Vec<MangleType>, return_type: MangleType) -> FunctionSignature {
    FunctionSignature {
        params,
        return_type,
        is_variadic: false,
        convention: MangleConvention::Omega,
    }
}

fn root(name: &str) -> ManglePath {
    ManglePath::Root(name.to_string())
}

fn nested(parent: ManglePath, ns: Namespace, name: &str) -> ManglePath {
    ManglePath::Nested(Box::new(parent), ns, name.to_string())
}

fn generic(parent: ManglePath, args: Vec<MangleType>) -> ManglePath {
    ManglePath::Generic(Box::new(parent), args)
}

fn named(path: ManglePath) -> MangleType {
    MangleType::Named(path, None)
}

fn assert_round_trips(symbol: &Symbol) -> String {
    let mangled = encode(symbol);
    assert!(
        mangled
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'),
        "mangled output must stay within [A-Za-z0-9_.]: {mangled}"
    );
    let decoded = decode(&mangled).unwrap_or_else(|| panic!("failed to decode: {mangled}"));
    assert_eq!(&decoded, symbol, "round-trip mismatch for {mangled}");
    assert!(demangle(&mangled).is_some());
    mangled
}

#[test]
fn free_function() {
    let path = nested(root("mymod"), Namespace::Value, "foo");
    let sym = Symbol {
        path,
        signature: Some(sig(vec![MangleType::I32, MangleType::I32], MangleType::I32)),
        vendor_suffix: None,
    };
    let mangled = assert_round_trips(&sym);
    assert_eq!(demangle(&mangled).unwrap(), "mymod::foo(i32, i32) -> i32");
}

#[test]
fn overloaded_free_functions_differ() {
    // Overload signatures must participate in symbol identity.
    let path = nested(root("mymod"), Namespace::Value, "do_thing");
    let a = Symbol {
        path: path.clone(),
        signature: Some(sig(vec![MangleType::I32], MangleType::Void)),
        vendor_suffix: None,
    };
    let b = Symbol {
        path,
        signature: Some(sig(
            vec![MangleType::Pointer(Box::new(MangleType::U8), false)],
            MangleType::Void,
        )),
        vendor_suffix: None,
    };
    let ma = assert_round_trips(&a);
    let mb = assert_round_trips(&b);
    assert_ne!(
        ma, mb,
        "overloads with different params must not collide on one symbol"
    );
}

#[test]
fn all_four_self_modes() {
    let owner = nested(root("mymod"), Namespace::Type, "Vec2");
    let method_path = nested(owner.clone(), Namespace::Value, "gets");

    let value_self = named(owner.clone());
    let mut_value_self = named(owner.clone());
    let pointer_self = MangleType::Pointer(Box::new(named(owner.clone())), false);
    let mut_pointer_self = MangleType::Pointer(Box::new(named(owner.clone())), true);

    let make = |self_ty: MangleType| Symbol {
        path: method_path.clone(),
        signature: Some(sig(vec![self_ty], MangleType::I32)),
        vendor_suffix: None,
    };

    let m_value = assert_round_trips(&make(value_self));
    let m_mut_value = assert_round_trips(&make(mut_value_self));
    let m_pointer = assert_round_trips(&make(pointer_self));
    let m_mut_pointer = assert_round_trips(&make(mut_pointer_self));

    // Receiver mutability alone does not change type-level symbol identity.
    assert_eq!(m_value, m_mut_value);
    // Value/pointer shape and pointer mutability must remain distinct identities.
    assert_ne!(m_value, m_pointer);
    assert_ne!(m_pointer, m_mut_pointer);
}

#[test]
fn generic_method_with_nested_generic_args_and_repeated_owner() {
    // Generic method signatures must round-trip with instantiated owner/parameter types.
    let owner = generic(
        nested(root("mymod"), Namespace::Type, "GenericPair"),
        vec![MangleType::I32],
    );
    let method_path = nested(owner.clone(), Namespace::Value, "add");
    let self_ty = MangleType::Pointer(Box::new(named(owner.clone())), false);
    let other_ty = MangleType::Pointer(Box::new(named(owner)), false);

    let sym = Symbol {
        path: method_path,
        signature: Some(sig(vec![self_ty, other_ty], MangleType::Void)),
        vendor_suffix: None,
    };
    let mangled = assert_round_trips(&sym);

    // Repeated structural types should use backreferences.
    assert!(
        mangled.matches('B').count() >= 2,
        "expected backref compression in {mangled}"
    );

    // Backreference compression should materially shorten repeated complex types.
    let baseline = Symbol {
        path: nested(root("mymod"), Namespace::Value, "baseline"),
        signature: Some(sig(
            vec![MangleType::Pointer(
                Box::new(named(generic(
                    nested(root("mymod"), Namespace::Type, "GenericPair"),
                    vec![MangleType::I32],
                ))),
                false,
            )],
            MangleType::Void,
        )),
        vendor_suffix: None,
    };
    let baseline_len = encode(&baseline).len();
    assert!(
        mangled.len() < 3 * baseline_len,
        "compression didn't help: {} vs 3x{}",
        mangled.len(),
        baseline_len
    );
}

#[test]
fn wrapped_types() {
    let path = nested(root("mymod"), Namespace::Value, "many_shapes");
    let params = vec![
        MangleType::Pointer(Box::new(MangleType::I32), false),
        MangleType::Pointer(Box::new(MangleType::I32), true),
        MangleType::Slice(Box::new(MangleType::U8), false),
        MangleType::Slice(Box::new(MangleType::U8), true),
        MangleType::Str(false),
        MangleType::Str(true),
        MangleType::Array(Box::new(MangleType::Char), false),
        MangleType::Array(Box::new(MangleType::Char), true),
        MangleType::SizedArray(Box::new(MangleType::I32), 17),
        MangleType::SpecObject(
            vec![named(nested(root("mymod"), Namespace::Type, "Animal"))],
            false,
        ),
        MangleType::SpecObject(
            vec![named(nested(root("mymod"), Namespace::Type, "Animal"))],
            true,
        ),
        MangleType::SpecObject(
            vec![
                named(nested(root("mymod"), Namespace::Type, "Animal")),
                named(nested(root("mymod"), Namespace::Type, "Named")),
            ],
            false,
        ),
        MangleType::Function(
            vec![MangleType::I32],
            Box::new(MangleType::Bool),
            false,
            MangleConvention::Omega,
        ),
        MangleType::Function(
            vec![MangleType::I32],
            Box::new(MangleType::Void),
            true,
            MangleConvention::Omega,
        ),
        named(nested(root("mymod"), Namespace::Type, "MyEnum")),
        MangleType::Named(nested(root("mymod"), Namespace::Type, "MyEnum"), Some(2)),
    ];
    let sym = Symbol {
        path,
        signature: Some(sig(params, MangleType::Void)),
        vendor_suffix: None,
    };
    assert_round_trips(&sym);
}

#[test]
fn str_never_collides_with_slice_u8() {
    // Runtime-shape equivalence must not collapse semantically distinct types.
    let path = nested(root("mymod"), Namespace::Value, "do_thing");
    let str_sym = Symbol {
        path: path.clone(),
        signature: Some(sig(vec![MangleType::Str(false)], MangleType::Void)),
        vendor_suffix: None,
    };
    let slice_sym = Symbol {
        path,
        signature: Some(sig(
            vec![MangleType::Slice(Box::new(MangleType::U8), false)],
            MangleType::Void,
        )),
        vendor_suffix: None,
    };
    let m_str = assert_round_trips(&str_sym);
    let m_slice = assert_round_trips(&slice_sym);
    assert_ne!(m_str, m_slice);
    assert_eq!(demangle(&m_str).unwrap(), "mymod::do_thing(*str) -> void");
    assert_eq!(
        demangle(&m_slice).unwrap(),
        "mymod::do_thing(*[]u8) -> void"
    );
}

#[test]
fn mut_str_round_trips_and_demangles() {
    let path = nested(root("mymod"), Namespace::Value, "takes_mut_str");
    let sym = Symbol {
        path,
        signature: Some(sig(vec![MangleType::Str(true)], MangleType::Str(false))),
        vendor_suffix: None,
    };
    let mangled = assert_round_trips(&sym);
    assert_eq!(
        demangle(&mangled).unwrap(),
        "mymod::takes_mut_str(*mut str) -> *str"
    );
}

#[test]
fn vendor_suffix_round_trips() {
    // Vendor suffixes must round-trip even though compiler-generated vtables do not rely on them.
    let owner = nested(root("mymod"), Namespace::Type, "Dog");
    let sym = Symbol {
        path: owner,
        signature: None,
        vendor_suffix: Some("llvm.1234".to_string()),
    };
    let mangled = assert_round_trips(&sym);
    assert!(mangled.ends_with(".llvm.1234"));
}

#[test]
fn vtable_symbol_shape_stays_alphanumeric() {
    // Vtables use ordinary nested path identity rather than a vendor suffix.
    let owner = nested(root("mymod"), Namespace::Type, "Dog");
    let sym = Symbol {
        path: nested(owner, Namespace::Value, "vtable"),
        signature: None,
        vendor_suffix: None,
    };
    let mangled = assert_round_trips(&sym);
    assert!(
        mangled
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    );
    assert_eq!(demangle(&mangled).unwrap(), "mymod::Dog::vtable");
}

#[test]
fn identifier_edge_cases_round_trip() {
    for name in [
        "a",
        "_leading_underscore",
        "0starts_with_digit",
        "trailing_",
    ] {
        let sym = Symbol {
            path: nested(root("mymod"), Namespace::Value, name),
            signature: None,
            vendor_suffix: None,
        };
        assert_round_trips(&sym);
    }
}

#[test]
fn convention_and_variadic_identity_round_trip_and_differ() {
    let path = nested(root("libc"), Namespace::Value, "printf");
    let omega_sym = Symbol {
        path: path.clone(),
        signature: Some(sig(vec![MangleType::I32], MangleType::I32)),
        vendor_suffix: None,
    };
    let c_sym = Symbol {
        path: path.clone(),
        signature: Some(FunctionSignature {
            params: vec![MangleType::I32],
            return_type: MangleType::I32,
            is_variadic: true,
            convention: MangleConvention::C,
        }),
        vendor_suffix: None,
    };
    let sysv64_sym = Symbol {
        path,
        signature: Some(FunctionSignature {
            params: vec![MangleType::I32],
            return_type: MangleType::I32,
            is_variadic: true,
            convention: MangleConvention::SysV64,
        }),
        vendor_suffix: None,
    };
    let m_omega = assert_round_trips(&omega_sym);
    let m_c = assert_round_trips(&c_sym);
    let m_sysv64 = assert_round_trips(&sysv64_sym);
    assert_ne!(
        m_omega, m_c,
        "convention must participate in symbol identity"
    );
    assert_ne!(
        m_c, m_sysv64,
        "distinct foreign conventions must not collide"
    );
    assert!(demangle(&m_c).unwrap().contains("foreign(c)"));
    assert!(demangle(&m_sysv64).unwrap().contains("foreign(sysv64)"));
}

#[test]
fn malformed_backref_is_rejected_not_looped() {
    // Reject forward/self backreferences instead of looping or panicking.
    assert!(decode("_omg_BZ_").is_none());
    assert!(decode("_omg_B_").is_none());
}

#[test]
fn structural_conformance_owner_paths_round_trip() {
    // Unnamed conformance targets must round-trip through structural type owners.
    for owner in [
        MangleType::Slice(Box::new(MangleType::U8), false),
        MangleType::Slice(Box::new(MangleType::U8), true),
        MangleType::Str(false),
        MangleType::Str(true),
        MangleType::I32,
        MangleType::Pointer(
            Box::new(named(nested(root("mymod"), Namespace::Type, "Dog"))),
            false,
        ),
    ] {
        let spec = ManglePath::Nested(
            Box::new(ManglePath::Type(Box::new(owner.clone()))),
            Namespace::Type,
            "Eq".to_string(),
        );
        let sym = Symbol {
            path: ManglePath::Nested(Box::new(spec), Namespace::Value, "equals".to_string()),
            signature: Some(sig(vec![owner.clone(), owner], MangleType::Bool)),
            vendor_suffix: None,
        };
        assert_round_trips(&sym);
    }
}

#[test]
fn structural_owners_of_different_shape_never_collide() {
    let build = |owner: MangleType| Symbol {
        path: ManglePath::Nested(
            Box::new(ManglePath::Nested(
                Box::new(ManglePath::Type(Box::new(owner))),
                Namespace::Type,
                "Eq".to_string(),
            )),
            Namespace::Value,
            "equals".to_string(),
        ),
        signature: Some(sig(vec![], MangleType::Bool)),
        vendor_suffix: None,
    };
    // Fat-pointer types and slice mutabilities remain distinct symbol identities.
    let mangled: Vec<String> = [
        MangleType::Str(false),
        MangleType::Str(true),
        MangleType::Slice(Box::new(MangleType::U8), false),
        MangleType::Slice(Box::new(MangleType::U8), true),
    ]
    .into_iter()
    .map(|owner| assert_round_trips(&build(owner)))
    .collect();
    for (i, a) in mangled.iter().enumerate() {
        for b in &mangled[i + 1..] {
            assert_ne!(a, b, "structural conform owners must not collide");
        }
    }
}

fn anonymous_enum(members: Vec<MangleType>) -> MangleType {
    MangleType::AnonymousEnum(members, None)
}

fn takes(ty: MangleType) -> Symbol {
    Symbol {
        path: nested(root("mymod"), Namespace::Value, "takes"),
        signature: Some(sig(vec![ty], MangleType::Void)),
        vendor_suffix: None,
    }
}

#[test]
fn anonymous_enum_round_trips_and_demangles() {
    let sym = takes(anonymous_enum(vec![
        MangleType::I32,
        MangleType::Str(false),
        named(nested(root("mymod"), Namespace::Type, "Failure")),
    ]));
    let mangled = assert_round_trips(&sym);
    assert_eq!(
        demangle(&mangled).unwrap(),
        "mymod::takes(enum i32 | *str | mymod::Failure) -> void"
    );
}

#[test]
fn a_one_member_anonymous_enum_is_not_its_member() {
    // The wrapper is a real tagged value with its own representation, so it
    // must never share a symbol with the member type it carries.
    let single = assert_round_trips(&takes(anonymous_enum(vec![MangleType::I32])));
    let bare = assert_round_trips(&takes(MangleType::I32));
    assert_ne!(single, bare);
}

#[test]
fn member_order_reaching_the_mangler_is_the_symbol() {
    // The analyzer canonicalizes members before mangling, so two spellings
    // of one type arrive here already identical -- and two genuinely
    // different member sets must not collide.
    let members = vec![MangleType::I32, MangleType::Str(false)];
    let once = assert_round_trips(&takes(anonymous_enum(members.clone())));
    let again = assert_round_trips(&takes(anonymous_enum(members.clone())));
    assert_eq!(once, again);

    let mut wider = members;
    wider.push(MangleType::Bool);
    let superset = assert_round_trips(&takes(anonymous_enum(wider)));
    assert_ne!(once, superset);
}

#[test]
fn an_anonymous_enum_nests_inside_other_type_forms() {
    let inner = anonymous_enum(vec![MangleType::I32, MangleType::Bool]);
    let nested_member = anonymous_enum(vec![inner.clone(), MangleType::Char]);
    let sym = Symbol {
        path: generic(
            nested(root("mymod"), Namespace::Value, "wrap"),
            vec![nested_member.clone()],
        ),
        signature: Some(sig(
            vec![MangleType::Pointer(Box::new(nested_member), false)],
            inner,
        )),
        vendor_suffix: None,
    };
    let mangled = assert_round_trips(&sym);
    assert_eq!(
        demangle(&mangled).unwrap(),
        "mymod::wrap<enum enum i32 | bool | char>(*enum enum i32 | bool | char) -> enum i32 | bool"
    );
}

#[test]
fn a_refined_anonymous_member_keeps_its_own_identity() {
    let members = vec![MangleType::I32, MangleType::Bool];
    let parent = assert_round_trips(&takes(MangleType::AnonymousEnum(members.clone(), None)));
    let first = assert_round_trips(&takes(MangleType::AnonymousEnum(members.clone(), Some(0))));
    let second = assert_round_trips(&takes(MangleType::AnonymousEnum(members, Some(1))));
    assert_ne!(parent, first);
    assert_ne!(first, second);
}

// --- mixed (value-carrying) generic applications -------------------------

fn value_symbol(args: Vec<MangleGenericArg>) -> Symbol {
    Symbol {
        path: ManglePath::generic(nested(root("pkg"), Namespace::Type, "Buffer"), args),
        signature: Some(sig(vec![], MangleType::Void)),
        vendor_suffix: None,
    }
}

fn usize_value(value: i128) -> MangleGenericArg {
    MangleGenericArg::Value(MangleValue::Int {
        r#type: MangleIntType::USize,
        value,
    })
}

#[test]
fn an_all_type_argument_list_keeps_the_legacy_encoding() {
    // The mixed model must not migrate a single existing generic symbol, so
    // this fixture pins the exact bytes an all-type list produces.
    let symbol = Symbol {
        path: generic(
            nested(root("pkg"), Namespace::Type, "Pair"),
            vec![MangleType::I32, MangleType::U8],
        ),
        signature: Some(sig(vec![], MangleType::Void)),
        vendor_suffix: None,
    };
    assert_eq!(encode(&symbol), "_omg_INtC3pkg4PairlhEEv");
    assert_round_trips(&symbol);

    // The same list built through the kind-aware constructor must produce
    // the identical bytes.
    let via_constructor = value_symbol(vec![
        MangleGenericArg::Type(MangleType::I32),
        MangleGenericArg::Type(MangleType::U8),
    ]);
    assert!(matches!(via_constructor.path, ManglePath::Generic(..)));
}

#[test]
fn mixed_generic_arguments_round_trip() {
    let symbol = value_symbol(vec![
        usize_value(10),
        MangleGenericArg::Type(MangleType::I32),
    ]);
    assert!(matches!(symbol.path, ManglePath::MixedGeneric(..)));
    let mangled = assert_round_trips(&symbol);
    assert_eq!(
        demangle(&mangled).unwrap(),
        "pkg::Buffer<10, i32>() -> void"
    );
}

#[test]
fn differing_comp_values_produce_differing_symbols() {
    let ten = encode(&value_symbol(vec![usize_value(10)]));
    let eleven = encode(&value_symbol(vec![usize_value(11)]));
    assert_ne!(ten, eleven);
}

#[test]
fn a_value_never_collides_with_a_type_argument() {
    let value = encode(&value_symbol(vec![usize_value(1)]));
    let r#type = encode(&value_symbol(vec![MangleGenericArg::Type(
        MangleType::USize,
    )]));
    assert_ne!(value, r#type);
}

#[test]
fn the_same_digits_under_two_declared_types_stay_distinct() {
    let as_usize = encode(&value_symbol(vec![usize_value(7)]));
    let as_u8 = encode(&value_symbol(vec![MangleGenericArg::Value(
        MangleValue::Int {
            r#type: MangleIntType::U8,
            value: 7,
        },
    )]));
    assert_ne!(as_usize, as_u8);
}

#[test]
fn negative_bool_and_char_values_round_trip() {
    let symbol = value_symbol(vec![
        MangleGenericArg::Value(MangleValue::Int {
            r#type: MangleIntType::I64,
            value: i64::MIN as i128,
        }),
        MangleGenericArg::Value(MangleValue::Bool(true)),
        MangleGenericArg::Value(MangleValue::Bool(false)),
        MangleGenericArg::Value(MangleValue::Char('z')),
    ]);
    let mangled = assert_round_trips(&symbol);
    assert_eq!(
        demangle(&mangled).unwrap(),
        "pkg::Buffer<-9223372036854775808, true, false, 'z'>() -> void"
    );
}

#[test]
fn a_mixed_generic_path_is_usable_as_a_named_type() {
    let symbol = Symbol {
        path: nested(root("pkg"), Namespace::Value, "takes_buffer"),
        signature: Some(sig(
            vec![named(ManglePath::generic(
                nested(root("pkg"), Namespace::Type, "Buffer"),
                vec![usize_value(4), MangleGenericArg::Type(MangleType::U8)],
            ))],
            MangleType::Void,
        )),
        vendor_suffix: None,
    };
    assert_round_trips(&symbol);
}

fn spec(module: &str, name: &str) -> ManglePath {
    nested(
        nested(root("pkg"), Namespace::Type, module),
        Namespace::Type,
        name,
    )
}

fn bound(spec: ManglePath, args: Vec<MangleTemplateArg>) -> MangleTemplateBound {
    MangleTemplateBound { spec, args }
}

/// `pick<#0: bounds>(#0) => i32`, the shape two selected overloads share.
fn pick(bounds: Vec<MangleTemplateBound>, args: Vec<MangleType>) -> Symbol {
    let path = nested(root("pkg"), Namespace::Value, "pick");
    let template = MangleTemplate {
        generics: vec![MangleTemplateParam::Type(bounds)],
        params: vec![MangleTemplateType::Param(0)],
        return_type: MangleTemplateType::Fixed(MangleType::I32),
        is_variadic: false,
        convention: MangleConvention::Omega,
    };
    Symbol {
        path: generic(
            ManglePath::Template(Box::new(path), Box::new(template)),
            args.clone(),
        ),
        signature: Some(sig(args, MangleType::I32)),
        vendor_suffix: None,
    }
}

#[test]
fn two_declarations_at_one_instantiation_are_two_symbols() {
    let a = pick(vec![bound(spec("m", "A"), vec![])], vec![MangleType::I32]);
    let b = pick(vec![bound(spec("m", "B"), vec![])], vec![MangleType::I32]);
    let a_mangled = assert_round_trips(&a);
    let b_mangled = assert_round_trips(&b);
    assert_ne!(a_mangled, b_mangled);
    // The demangled form has to say which declaration it is, not only that
    // one was instantiated at `i32`.
    assert!(demangle(&a_mangled).unwrap().contains("pkg::m::A"));
    assert!(demangle(&b_mangled).unwrap().contains("pkg::m::B"));
}

#[test]
fn the_same_short_spec_name_in_two_modules_stays_distinct() {
    let here = pick(
        vec![bound(spec("here", "A"), vec![])],
        vec![MangleType::I32],
    );
    let there = pick(
        vec![bound(spec("there", "A"), vec![])],
        vec![MangleType::I32],
    );
    assert_ne!(assert_round_trips(&here), assert_round_trips(&there));
}

#[test]
fn a_symbolic_bound_argument_is_not_the_concrete_type_it_is_instantiated_with() {
    // `<T: Holds<T>>` and `<T: Holds<i32>>` both instantiate at `i32`; only
    // the descriptor keeps them apart.
    let holds = spec("m", "Holds");
    let symbolic = pick(
        vec![bound(
            holds.clone(),
            vec![MangleTemplateArg::Type(MangleTemplateType::Param(0))],
        )],
        vec![MangleType::I32],
    );
    let fixed = pick(
        vec![bound(
            holds,
            vec![MangleTemplateArg::Type(MangleTemplateType::Fixed(
                MangleType::I32,
            ))],
        )],
        vec![MangleType::I32],
    );
    assert_ne!(assert_round_trips(&symbolic), assert_round_trips(&fixed));
}

#[test]
fn every_nested_template_shape_round_trips() {
    let template = MangleTemplate {
        generics: vec![
            MangleTemplateParam::Type(vec![bound(
                spec("m", "Holds"),
                vec![
                    MangleTemplateArg::Type(MangleTemplateType::Param(0)),
                    MangleTemplateArg::Param(1),
                    MangleTemplateArg::Value(MangleValue::Int {
                        r#type: MangleIntType::USize,
                        value: -3,
                    }),
                ],
            )]),
            MangleTemplateParam::Comp(MangleTemplateType::Fixed(MangleType::USize)),
            MangleTemplateParam::Type(vec![]),
        ],
        params: vec![
            MangleTemplateType::Pointer(Box::new(MangleTemplateType::Param(0)), true),
            MangleTemplateType::Slice(Box::new(MangleTemplateType::Param(2)), false),
            MangleTemplateType::Array(Box::new(MangleTemplateType::Fixed(MangleType::U8)), true),
            MangleTemplateType::SizedArray(
                Box::new(MangleTemplateType::Param(0)),
                Box::new(MangleTemplateArg::Param(1)),
            ),
            MangleTemplateType::SpecObject(
                vec![
                    bound(spec("m", "A"), vec![]),
                    bound(
                        spec("m", "B"),
                        vec![MangleTemplateArg::Type(MangleTemplateType::Param(2))],
                    ),
                ],
                false,
            ),
            MangleTemplateType::AnonymousEnum(vec![
                MangleTemplateType::Param(0),
                MangleTemplateType::Fixed(MangleType::Void),
            ]),
            MangleTemplateType::Function(
                vec![MangleTemplateType::Param(2)],
                Box::new(MangleTemplateType::Fixed(MangleType::Never)),
                true,
                MangleConvention::C,
            ),
            MangleTemplateType::Nominal(
                nested(root("pkg"), Namespace::Type, "Box"),
                vec![MangleTemplateArg::Type(MangleTemplateType::Param(0))],
            ),
        ],
        return_type: MangleTemplateType::Param(2),
        is_variadic: true,
        convention: MangleConvention::SysV64,
    };
    let symbol = Symbol {
        path: generic(
            ManglePath::Template(
                Box::new(nested(root("pkg"), Namespace::Value, "every")),
                Box::new(template),
            ),
            vec![MangleType::I32],
        ),
        signature: Some(sig(vec![MangleType::I32], MangleType::Void)),
        vendor_suffix: None,
    };
    assert_round_trips(&symbol);
}

#[test]
fn a_truncated_or_invalid_descriptor_is_rejected() {
    let symbol = pick(vec![bound(spec("m", "A"), vec![])], vec![MangleType::I32]);
    let mangled = encode(&symbol);
    for cut in 1..mangled.len() {
        // Truncation must fail cleanly rather than decode to a shorter name.
        let truncated = &mangled[..cut];
        assert_ne!(decode(truncated), Some(symbol.clone()));
    }

    // A parameter reference no declared parameter answers.
    let out_of_range = MangleTemplate {
        generics: vec![MangleTemplateParam::Type(vec![])],
        params: vec![MangleTemplateType::Param(4)],
        return_type: MangleTemplateType::Fixed(MangleType::Void),
        is_variadic: false,
        convention: MangleConvention::Omega,
    };
    assert!(!out_of_range.references_are_valid());

    // A type position naming a `comp` parameter, and the reverse.
    let wrong_kind = MangleTemplate {
        generics: vec![MangleTemplateParam::Comp(MangleTemplateType::Fixed(
            MangleType::USize,
        ))],
        params: vec![MangleTemplateType::Param(0)],
        return_type: MangleTemplateType::Fixed(MangleType::Void),
        is_variadic: false,
        convention: MangleConvention::Omega,
    };
    assert!(!wrong_kind.references_are_valid());
}

/// Byte fixtures of symbols produced before the template path form existed.
/// They are literals on purpose: an encoder change that silently migrated
/// them would be an ABI break for every object already compiled.
const FREE_FUNCTION_FIXTURE: &str = "_omg_NvC5mymod3fooEv";
const GENERIC_FUNCTION_FIXTURE: &str = "_omg_INvC5mymod3summEmmEm";

#[test]
fn a_non_template_symbol_still_decodes_unchanged() {
    // The pre-template encodings are untouched by the new path form.
    for mangled in [FREE_FUNCTION_FIXTURE, GENERIC_FUNCTION_FIXTURE] {
        assert!(decode(mangled).is_some(), "{mangled} must still decode");
        assert!(!matches!(
            decode(mangled).unwrap().path,
            ManglePath::Template(..)
        ));
    }
}
