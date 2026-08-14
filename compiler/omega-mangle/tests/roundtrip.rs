//! Round-trip and compression coverage across every grammar production
//! (see the plan's Verification §1). `omega-mangle` never depends on
//! `omega-analyzer`, so these `Symbol`s are hand-built rather than
//! derived from real `ResolvedType`s -- exactly how `omega-codegen` will
//! build them in practice.

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

/// Every mangled symbol must round-trip through `decode` back to a
/// structurally identical `Symbol`, and `demangle` must succeed (a
/// round-trip failure would mean the encoder and decoder have drifted
/// out of sync with each other).
fn assert_round_trips(symbol: &Symbol) -> String {
    let mangled = encode(symbol);
    assert!(
        mangled.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'),
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
    // Same path, different params -- the deliberate deviation from RFC
    // 2603 (which never needs this, since Rust has no overloading).
    let path = nested(root("mymod"), Namespace::Value, "do_thing");
    let a = Symbol { path: path.clone(), signature: Some((vec![MangleType::I32], MangleType::Void)), vendor_suffix: None };
    let b = Symbol {
        path,
        signature: Some((vec![MangleType::Pointer(Box::new(MangleType::U8), false)], MangleType::Void)),
        vendor_suffix: None,
    };
    let ma = assert_round_trips(&a);
    let mb = assert_round_trips(&b);
    assert_ne!(ma, mb, "overloads with different params must not collide on one symbol");
}

#[test]
fn all_four_self_modes() {
    let owner = nested(root("mymod"), Namespace::Type, "Vec2");
    let method_path = nested(owner.clone(), Namespace::Value, "gets");

    let value_self = named(owner.clone());
    let mut_value_self = named(owner.clone()); // `mut self` desugars identically -- see below
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

    // `self` and `mut self` are indistinguishable at the type level (both
    // by-value; mutability of a by-value self is a local-binding property
    // of the synthesized shadow, never the parameter's own type) -- this
    // is documented, provably harmless collision (AmbiguousSelfOverload
    // already forbids the two from ever legally coexisting on one
    // signature), not a bug.
    assert_eq!(m_value, m_mut_value);
    // But value vs. pointer, and immutable vs. mutable pointer, must all
    // differ -- these genuinely are distinct, independently linkable
    // functions.
    assert_ne!(m_value, m_pointer);
    assert_ne!(m_pointer, m_mut_pointer);
}

#[test]
fn generic_method_with_nested_generic_args_and_repeated_owner() {
    // <mymod::GenericPair<i32>>::add(*self, other: *mymod::GenericPair<i32>) -> void
    let owner = generic(nested(root("mymod"), Namespace::Type, "GenericPair"), vec![MangleType::I32]);
    let method_path = nested(owner.clone(), Namespace::Value, "add");
    let self_ty = MangleType::Pointer(Box::new(named(owner.clone())), false);
    let other_ty = MangleType::Pointer(Box::new(named(owner)), false);

    let sym = Symbol { path: method_path, signature: Some((vec![self_ty, other_ty], MangleType::Void)), vendor_suffix: None };
    let mangled = assert_round_trips(&sym);

    // `other`'s type is structurally identical to `self`'s, and the
    // owner path itself was already fully spelled out once while
    // encoding the symbol's own path -- both should collapse to
    // backrefs, so the mangled form must contain at least two of them.
    assert!(mangled.matches('B').count() >= 2, "expected backref compression in {mangled}");

    // Compare against the cost of a single occurrence of the same owner
    // type to confirm compression is actually paying off, not just
    // present: three real occurrences of `*GenericPair<i32>` (self,
    // other's pointee via path-level reuse, and the path itself) should
    // cost far less than three independent full spellings would.
    let baseline = Symbol {
        path: nested(root("mymod"), Namespace::Value, "baseline"),
        signature: Some((vec![MangleType::Pointer(Box::new(named(generic(
            nested(root("mymod"), Namespace::Type, "GenericPair"),
            vec![MangleType::I32],
        ))), false)], MangleType::Void)),
        vendor_suffix: None,
    };
    let baseline_len = encode(&baseline).len();
    assert!(mangled.len() < 3 * baseline_len, "compression didn't help: {} vs 3x{}", mangled.len(), baseline_len);
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
        MangleType::SpecObject(Box::new(named(nested(root("mymod"), Namespace::Type, "Animal"))), false),
        MangleType::SpecObject(Box::new(named(nested(root("mymod"), Namespace::Type, "Animal"))), true),
        MangleType::Function(vec![MangleType::I32], Box::new(MangleType::Bool), false),
        MangleType::Function(vec![MangleType::I32], Box::new(MangleType::Void), true),
        named(nested(root("mymod"), Namespace::Type, "MyEnum")),
        MangleType::Named(nested(root("mymod"), Namespace::Type, "MyEnum"), Some(2)),
    ];
    let sym = Symbol { path, signature: Some((params, MangleType::Void)), vendor_suffix: None };
    assert_round_trips(&sym);
}

#[test]
fn str_never_collides_with_slice_u8() {
    // `*str` and `*[]u8` share an identical runtime shape but must never
    // mangle to the same symbol -- otherwise two overloads differing only
    // in one taking `*str` and the other `*[]u8` would collide.
    let path = nested(root("mymod"), Namespace::Value, "do_thing");
    let str_sym = Symbol {
        path: path.clone(),
        signature: Some((vec![MangleType::Str(false)], MangleType::Void)),
        vendor_suffix: None,
    };
    let slice_sym = Symbol {
        path,
        signature: Some((vec![MangleType::Slice(Box::new(MangleType::U8), false)], MangleType::Void)),
        vendor_suffix: None,
    };
    let m_str = assert_round_trips(&str_sym);
    let m_slice = assert_round_trips(&slice_sym);
    assert_ne!(m_str, m_slice);
    assert_eq!(demangle(&m_str).unwrap(), "mymod::do_thing(*str) -> void");
    assert_eq!(demangle(&m_slice).unwrap(), "mymod::do_thing(*[]u8) -> void");
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
    assert_eq!(demangle(&mangled).unwrap(), "mymod::takes_mut_str(*mut str) -> *str");
}

#[test]
fn vendor_suffix_round_trips() {
    // The general RFC-2603-style escape hatch (for external tooling, e.g.
    // an LTO pass) -- omega_codegen's own vtable symbols deliberately do
    // *not* use this (see its `vtable_symbol` doc comment), but the
    // mechanism itself must still work correctly.
    let owner = nested(root("mymod"), Namespace::Type, "Dog");
    let sym = Symbol { path: owner, signature: None, vendor_suffix: Some("llvm.1234".to_string()) };
    let mangled = assert_round_trips(&sym);
    assert!(mangled.ends_with(".llvm.1234"));
}

#[test]
fn vtable_symbol_shape_stays_alphanumeric() {
    // omega_codegen mangles a vtable as an ordinary nested `vtable`
    // identifier under the owner type, not a vendor suffix -- confirm
    // that shape round-trips and never needs the `.` escape hatch.
    let owner = nested(root("mymod"), Namespace::Type, "Dog");
    let sym = Symbol { path: nested(owner, Namespace::Value, "vtable"), signature: None, vendor_suffix: None };
    let mangled = assert_round_trips(&sym);
    assert!(mangled.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    assert_eq!(demangle(&mangled).unwrap(), "mymod::Dog::vtable");
}

#[test]
fn identifier_edge_cases_round_trip() {
    for name in ["a", "_leading_underscore", "0starts_with_digit", "trailing_"] {
        let sym = Symbol { path: nested(root("mymod"), Namespace::Value, name), signature: None, vendor_suffix: None };
        assert_round_trips(&sym);
    }
}

#[test]
fn malformed_backref_is_rejected_not_looped() {
    // A backref pointing forward (or at/past itself) can never occur in
    // honest output -- a decoder must reject it outright rather than
    // recursing forever or panicking on adversarial/corrupted input.
    assert!(decode("_omg_BZ_").is_none());
    assert!(decode("_omg_B_").is_none());
}

#[test]
fn structural_conformance_owner_paths_round_trip() {
    // `omega_codegen::mangle::conformance_method_symbol` wraps an unnamed
    // conformance target (`conform []u8 to Eq`, `conform str to Eq`,
    // `conform i32 to Ord`) in `ManglePath::Type` rather than rendering the
    // type through `Display`. Every such owner must stay inside
    // `[A-Za-z0-9_]` and decode back to the same structural type -- the
    // whole point of the wrapper.
    for owner in [
        MangleType::Slice(Box::new(MangleType::U8), false),
        MangleType::Slice(Box::new(MangleType::U8), true),
        MangleType::Str(false),
        MangleType::Str(true),
        MangleType::I32,
        MangleType::Pointer(Box::new(named(nested(root("mymod"), Namespace::Type, "Dog"))), false),
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
    // `*str` and `*[?]u8` are the two fat pointers, and previously both
    // rendered through `Display` into a path segment; they must stay
    // distinct symbols, as must a slice's two mutabilities.
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
