use omega_mangle::{ManglePath, MangleType, Namespace, Symbol, decode, demangle, encode};

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
        signature: Some((vec![MangleType::I32, MangleType::I32], MangleType::I32)),
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
        signature: Some((vec![MangleType::I32], MangleType::Void)),
        vendor_suffix: None,
    };
    let b = Symbol {
        path,
        signature: Some((
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
        signature: Some((vec![self_ty], MangleType::I32)),
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
        signature: Some((vec![self_ty, other_ty], MangleType::Void)),
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
        signature: Some((
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
            Box::new(named(nested(root("mymod"), Namespace::Type, "Animal"))),
            false,
        ),
        MangleType::SpecObject(
            Box::new(named(nested(root("mymod"), Namespace::Type, "Animal"))),
            true,
        ),
        MangleType::Function(vec![MangleType::I32], Box::new(MangleType::Bool), false),
        MangleType::Function(vec![MangleType::I32], Box::new(MangleType::Void), true),
        named(nested(root("mymod"), Namespace::Type, "MyEnum")),
        MangleType::Named(nested(root("mymod"), Namespace::Type, "MyEnum"), Some(2)),
    ];
    let sym = Symbol {
        path,
        signature: Some((params, MangleType::Void)),
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
        signature: Some((vec![MangleType::Str(false)], MangleType::Void)),
        vendor_suffix: None,
    };
    let slice_sym = Symbol {
        path,
        signature: Some((
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
        signature: Some((vec![MangleType::Str(true)], MangleType::Str(false))),
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
            signature: Some((vec![owner.clone(), owner], MangleType::Bool)),
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
        signature: Some((vec![], MangleType::Bool)),
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
