use omega_analyzer::Target;
use omega_analyzer::checked::{CheckedItem, ExternFunctionKind, NumberValue};
use omega_analyzer::error::{AnalysisErrorKind, TypeResolutionError};
use omega_analyzer::resolved_type::{ConstValue, FunctionNamespace};
use omega_analyzer::resolver::ResolveError;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::{macros::MacroError, prelude::Ident};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_conform_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn write_child(&self, name: &str, source: &str) {
        fs::write(self.0.join(format!("{name}.omg")), source).expect("write test child module");
    }

    /// Writes a module at an arbitrary logical depth, e.g.
    /// `write_nested("outer/inner", ...)` creates a directory-shaped `outer`
    /// with a file-shaped child `outer::inner`.
    fn write_nested(&self, relative: &str, source: &str) {
        let path = self.0.join(format!("{relative}.omg"));
        fs::create_dir_all(path.parent().expect("nested module has a parent"))
            .expect("create nested module directory");
        fs::write(path, source).expect("write nested test module");
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::new(), Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let parent = self.0.parent().expect("test root has a parent");
        let _ = fs::remove_dir_all(parent);
    }
}

fn has_analysis_error(
    errors: &[CompileError],
    predicate: impl Fn(&AnalysisErrorKind) -> bool,
) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| predicate(&error.kind)),
        _ => false,
    })
}

fn compile_errors(package: &TestPackage, message: &str) -> Vec<CompileError> {
    match package.compile() {
        Ok(_) => panic!("{message}"),
        Err(errors) => errors,
    }
}

fn option_core() -> TestPackage {
    TestPackage::new("exposed enum Option<T> { None, Some { exposed value: T; }; }")
}

#[test]
fn bound_and_spec_qualified_dispatch_compile() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        meet Speak for Dog { speak(*self) => i32 { self.value } }

        call_bound<T: Speak>(value: *T) => i32 { value.speak() }
        entry_fn() => i32 {
            dog := Dog { value = 7; };
            call_bound(&dog) + Speak::speak(&dog)
        }
        "#,
    );
    package
        .compile()
        .expect("both conformance call forms should compile");
}

#[test]
fn a_conjunction_bound_requires_every_member() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        struct Half { exposed value: i32; }
        meet A for Half { a(*self) => i32 { self.value } }

        use_both<T: A + B>(value: *T) => i32 { value.a() + value.b() }
        entry_fn() => i32 {
            half := Half { value = 1; };
            use_both(&half)
        }
        "#,
    );
    let errors = compile_errors(&package, "a conjunction bound must require both specs");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
            spec, ..
        }) if spec.as_ref() == "B"
    )));
}

#[test]
fn a_three_way_conjunction_bound_requires_all_members() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec C { c(*self) => i32; }
        struct Full { exposed value: i32; }
        meet A for Full { a(*self) => i32 { self.value } }
        meet B for Full { b(*self) => i32 { self.value } }
        meet C for Full { c(*self) => i32 { self.value } }

        use_all<T: A + B + C>(value: *T) => i32 { value.a() + value.b() + value.c() }
        entry_fn() => i32 {
            full := Full { value = 1; };
            use_all(&full)
        }
        "#,
    );
    package
        .compile()
        .expect("a type conforming to all three members must satisfy the conjunction");
}

#[test]
fn a_bound_spelled_either_member_order_admits_the_same_types() {
    let source = r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        struct Full { exposed value: i32; }
        meet A for Full { a(*self) => i32 { self.value } }
        meet B for Full { b(*self) => i32 { self.value } }

        use_ab<T: A + B>(value: *T) => i32 { value.a() + value.b() }
        use_ba<T: B + A>(value: *T) => i32 { value.a() + value.b() }
        entry_fn() => i32 {
            full := Full { value = 1; };
            use_ab(&full) + use_ba(&full)
        }
        "#;
    let package = TestPackage::new(source);
    package
        .compile()
        .expect("a conjunction bound admits the same types regardless of member order");

    let missing_b = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        struct Half { exposed value: i32; }
        meet A for Half { a(*self) => i32 { self.value } }

        use_ba<T: B + A>(value: *T) => i32 { value.a() }
        entry_fn() => i32 {
            half := Half { value = 1; };
            use_ba(&half)
        }
        "#,
    );
    let errors = compile_errors(
        &missing_b,
        "a type conforming to only one member must not satisfy the conjunction",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
            spec,
            missing,
            ..
        }) if spec.as_ref() == "B" && missing.contains(&Ident("b".to_string()))
    )));
}

#[test]
fn a_conjunction_with_a_repeated_member_collapses_to_the_member() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        struct S { exposed v: i32; }
        meet A for S { a(*self) => i32 { self.v } }

        use_repeated<T: A + A>(x: *T) => i32 { x.a() }
        via_object(x: *spec A + A) => i32 { x.a() }
        entry_fn() => i32 {
            s := S { v = 5; };
            use_repeated(&s) + via_object(&s)
        }
        "#,
    );
    package
        .compile()
        .expect("A + A collapses to A: a repeated member is not a distinct requirement");
}

#[test]
fn a_blanket_bounded_on_a_conjunction_applies_conditionally() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        meet<T: A + B> Sum for T {
            sum(*self) => i32 { self.a() + self.b() }
        }
        struct Full { exposed value: i32; }
        meet A for Full { a(*self) => i32 { self.value } }
        meet B for Full { b(*self) => i32 { self.value } }

        use_sum<T: Sum>(value: *T) => i32 { value.sum() }
        entry_fn() => i32 {
            full := Full { value = 2; };
            use_sum(&full)
        }
        "#,
    );
    package
        .compile()
        .expect("a blanket bounded on a conjunction must apply to a type with both");
}

#[test]
fn same_named_spec_functions_keep_their_own_spec_identity() {
    let package = TestPackage::new(
        r#"
        exposed spec A { tag(*self) => i32; }
        exposed spec B { tag(*self) => i32; }
        struct Thing { exposed a: i32; exposed b: i32; }
        meet A for Thing { tag(*self) => i32 { self.a } }
        meet B for Thing { tag(*self) => i32 { self.b } }

        via_a<T: A>(x: *T) => i32 { x.tag() }
        via_b<T: B>(x: *T) => i32 { x.tag() }
        entry_fn() => i32 {
            t := Thing { a = 1; b = 2; };
            via_a(&t) + via_b(&t)
        }
        "#,
    );
    package
        .compile()
        .expect("each spec's same-named function must resolve to its own conform");
}

#[test]
fn a_colliding_method_on_a_conjunction_object_is_ambiguous() {
    let package = TestPackage::new(
        r#"
        exposed spec A { tag(*self) => i32; }
        exposed spec B { tag(*self) => i32; }
        struct Thing { exposed a: i32; exposed b: i32; }
        meet A for Thing { tag(*self) => i32 { self.a } }
        meet B for Thing { tag(*self) => i32 { self.b } }

        via_ab(x: *spec A + B) => i32 { x.tag() }
        entry_fn() => i32 {
            t := Thing { a = 1; b = 2; };
            via_ab(&t)
        }
        "#,
    );
    let errors = compile_errors(
        &package,
        "a colliding method through a conjunction object must not silently pick",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::AmbiguousSpecObjectMethod { function, specs }
            if function.as_ref() == "tag" && specs.len() == 2
    )));
}

#[test]
fn a_narrowing_cast_disambiguates_a_conjunction_object() {
    let package = TestPackage::new(
        r#"
        exposed spec A { tag(*self) => i32; }
        exposed spec B { tag(*self) => i32; }
        struct Thing { exposed a: i32; exposed b: i32; }
        meet A for Thing { tag(*self) => i32 { self.a } }
        meet B for Thing { tag(*self) => i32 { self.b } }

        via_a(x: *spec A + B) => i32 { (<*spec A>x).tag() }
        via_b(x: *spec A + B) => i32 { (<*spec B>x).tag() }
        entry_fn() => i32 {
            t := Thing { a = 1; b = 2; };
            via_a(&t) + via_b(&t)
        }
        "#,
    );
    package
        .compile()
        .expect("a narrowing cast onto either member's section must compile");
}

#[test]
fn a_spec_object_type_is_identical_regardless_of_member_order() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        struct Full { exposed value: i32; }
        meet A for Full { a(*self) => i32 { self.value } }
        meet B for Full { b(*self) => i32 { self.value } }

        via_a_b(x: *spec A + B) => i32 { x.a() + x.b() }
        via_b_a(x: *spec B + A) => i32 { x.a() + x.b() }
        entry_fn() => i32 {
            full := Full { value = 3; };
            obj : *spec A + B = &full;
            via_a_b(obj) + via_b_a(obj)
        }
        "#,
    );
    package.compile().expect(
        "*spec A + B and *spec B + A are exactly the same type and need no cast between them",
    );
}

#[test]
fn widening_and_unrelated_spec_object_casts_are_rejected() {
    let package = TestPackage::new(
        r#"
        exposed spec A { tag(*self) => i32; }
        exposed spec B { tag(*self) => i32; }
        exposed spec C { tag(*self) => i32; }
        struct Thing { exposed a: i32; }
        meet A for Thing { tag(*self) => i32 { self.a } }
        meet B for Thing { tag(*self) => i32 { self.a } }
        meet C for Thing { tag(*self) => i32 { self.a } }

        widen(x: *spec A) => *spec A + B { <*spec A + B>x }
        entry_fn() => i32 {
            t := Thing { a = 1; };
            widen(&t).tag()
        }
        "#,
    );
    let errors = compile_errors(&package, "a widening spec-object cast must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::SpecObjectCastImpossible { .. }
    )));
}

#[test]
fn a_fully_qualified_spec_call_resolves_an_ambiguous_static() {
    let package = TestPackage::new(
        r#"
        exposed spec P { make() => Self; label(*self) => i32; }
        exposed spec Q { make() => Self; label(*self) => i32; }
        struct S { exposed v: i32; }
        meet P for S {
            make() => Self { S { v = 1; } }
            label(*self) => i32 { self.v }
        }
        meet Q for S {
            make() => Self { S { v = 2; } }
            label(*self) => i32 { self.v }
        }
        via_p() => i32 { (<S : P>::make()).v }
        via_q() => i32 { (<S : Q>::make()).v }
        label_p(s: *S) => i32 { <S : P>::label(s) }
        entry_fn() => i32 {
            s := S { v = 3; };
            via_p() + via_q() + label_p(&s)
        }
        "#,
    );
    package
        .compile()
        .expect("the fully-qualified spelling works for statics and instance methods");
}

#[test]
fn an_ambiguous_conforming_static_names_the_candidates_and_their_spelling() {
    let package = TestPackage::new(
        r#"
        exposed spec P { make() => Self; }
        exposed spec Q { make() => Self; }
        struct S { exposed v: i32; }
        meet P for S { make() => Self { S { v = 1; } } }
        meet Q for S { make() => Self { S { v = 2; } } }
        entry_fn() => i32 { (S::make()).v }
        "#,
    );
    let errors = compile_errors(&package, "an ambiguous conforming static must be diagnosed");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::AmbiguousConformanceFunction { specs, namespace, .. }
            if specs.len() == 2 && *namespace == FunctionNamespace::Static
    )));
}

#[test]
fn fully_qualified_spec_call_negatives_name_their_cause() {
    let not_a_spec = TestPackage::new(
        r#"
        exposed spec P { make() => Self; }
        struct S { exposed v: i32; }
        struct NotASpec { exposed v: i32; }
        meet P for S { make() => Self { S { v = 1; } } }
        entry_fn() => i32 { (<S : NotASpec>::make()).v }
        "#,
    );
    let errors = compile_errors(
        &not_a_spec,
        "a non-spec in the qualified pair must be named",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name))
            if name.as_ref() == "NotASpec"
    )));

    let no_conform = TestPackage::new(
        r#"
        exposed spec P { make() => Self; }
        struct S { exposed v: i32; }
        entry_fn() => i32 { (<S : P>::make()).v }
        "#,
    );
    let errors = compile_errors(
        &no_conform,
        "a target that doesn't conform must report the missing conformance",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ModuleResolution(ResolveError::SpecNotImplemented {
            type_name,
            spec,
            ..
        }) if type_name == "S" && spec.as_ref() == "P"
    )));

    let no_function = TestPackage::new(
        r#"
        exposed spec P { make() => Self; }
        struct S { exposed v: i32; }
        meet P for S { make() => Self { S { v = 1; } } }
        entry_fn() => i32 { (<S : P>::nonexistent()).v }
        "#,
    );
    let errors = compile_errors(
        &no_function,
        "a function the spec lacks must name the spec that lacks it",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::NoSuchSpecFunction { spec, function }
            if spec.as_ref() == "P" && function.as_ref() == "nonexistent"
    )));
}

#[test]
fn a_receiverless_spec_call_without_an_expected_type_says_self_is_undetermined() {
    let package = TestPackage::new(
        r#"
        exposed spec Bounded { min() => Self; max() => Self; }
        struct S { exposed v: i32; }
        meet Bounded for S {
            min() => Self { S { v = 0; } }
            max() => Self { S { v = 1; } }
        }
        entry_fn() => i32 { x := Bounded::min(); x.v }
        "#,
    );
    let errors = compile_errors(
        &package,
        "a receiverless spec call with no expected type must not report an argument count",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::SpecStaticNeedsExpectedType { spec, function }
            if spec.as_ref() == "Bounded" && function.as_ref() == "min"
    )));
    assert!(!has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::WrongArgumentCount { .. }
    )));
}

#[test]
fn a_receiverless_spec_call_with_a_non_self_return_is_uninferable() {
    let package = TestPackage::new(
        r#"
        exposed spec F { n() => usize; }
        struct S { exposed v: i32; }
        meet F for S { n() => usize { 7usize } }
        entry_fn() => i32 { x : usize = F::n(); <i32>x }
        "#,
    );
    let errors = compile_errors(
        &package,
        "a return type that doesn't name Self must be rejected, not guessed",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::SpecStaticReturnNotSelf { spec, function, .. }
            if spec.as_ref() == "F" && function.as_ref() == "n"
    )));
}

#[test]
fn a_receiverless_spec_call_takes_self_from_the_expected_type() {
    let package = TestPackage::new(
        r#"
        exposed spec Bounded { min() => Self; max() => Self; }
        struct S { exposed v: i32; }
        meet Bounded for S {
            min() => Self { S { v = 0; } }
            max() => Self { S { v = 1; } }
        }
        takes(s: S) => i32 { s.v }
        entry_fn() => i32 {
            lo : S = Bounded::min();
            hi := takes(Bounded::max());
            lo.v + hi
        }
        "#,
    );
    package
        .compile()
        .expect("a receiverless spec call resolves Self from the expected type");
}

#[test]
fn conforming_instance_method_is_not_in_concrete_scope() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        meet Speak for Dog { speak(*self) => i32 { self.value } }
        entry_fn() => i32 { dog := Dog { value = 7; }; dog.speak() }
        "#,
    );
    let errors = compile_errors(&package, "concrete instance syntax must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn duplicate_and_extra_conformances_are_rejected() {
    let duplicate = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        meet Speak for Dog { speak(*self) => i32 { self.value } }
        meet Speak for Dog { speak(*self) => i32 { self.value } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&duplicate, "duplicate conformance must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::DuplicateConformance { .. }
    )));

    let extra = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        meet Speak for Dog {
            speak(*self) => i32 { self.value }
            extra(*self) => i32 { 0 }
        }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&extra, "extra conform functions must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ConformanceExtraFunction { .. }
    )));
}

#[test]
fn a_conform_cannot_borrow_an_inherent_requirement() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog {
            exposed value: i32;
            exposed speak(*self) => i32 { self.value }
        }
        meet Speak for Dog {}
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "an inherent method must not satisfy conform");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MissingSpecFunction { .. }
    )));
}

#[test]
fn slice_conformances_and_invalid_structural_targets_are_diagnosed_semantically() {
    let slice = TestPackage::new(
        r#"
        exposed spec Empty { empty(*self) => bool; }
        meet Empty for []u8 { empty(*self) => bool { self.length == 0 } }
        entry_fn() => i32 { 0 }
        "#,
    );
    slice
        .compile()
        .expect("a bare slice target should reach the conform registry");

    let pointer = TestPackage::new(
        r#"
        exposed spec Empty { empty(*self) => bool; }
        struct Dog { exposed value: i32; }
        meet Empty for *Dog { empty(*self) => bool { false } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(
        &pointer,
        "a pointer target must be rejected by the target model",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ConformTargetNotAType
    )));
}

#[test]
fn dependency_conformances_satisfy_the_dependency_bound() {
    let package = TestPackage::new(
        r#"
        exposed spec Animal { sound(*self) => i32; }
        exposed spec Mammal { fur(*self) => i32; }
        struct Dog { exposed value: i32; }
        meet Animal for Dog { sound(*self) => i32 { self.value } }
        meet Mammal for Dog { fur(*self) => i32 { 1 } }
        call<T: Animal>(value: *T) => i32 { value.sound() }
        entry_fn() => i32 { dog := Dog { value = 4; }; call(&dog) }
        "#,
    );
    package
        .compile()
        .expect("a bound is satisfied by the conform named for its own spec");
}

#[test]
fn primitive_blocks_are_core_only() {
    let package = TestPackage::new(
        r#"
        primitive i32 { exposed identity(*self) => i32 { *self } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "non-core primitive block must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::PrimitiveOutsideCore
    )));
}

#[test]
fn external_non_generic_primitive_is_imported_not_redefined() {
    let core = TestPackage::new("primitive i32 { exposed identity(*self) => i32 { *self } }");
    let local = TestPackage::new("entry_fn() => i32 { 7i32.identity() }");
    let mut driver = Driver::new(
        local.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with core extern");
    let program = driver
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
        .expect("external primitive use should compile");

    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(definitions, 1, "only local main should be defined");
    assert!(
        program
            .extern_functions
            .iter()
            .any(|function| matches!(function.kind, ExternFunctionKind::Primitive { .. }))
    );
}

#[test]
fn extern_generic_instantiation_keeps_its_declaring_module() {
    let library = TestPackage::new(
        r#"
        exposed identity<T>(value: T) => T { value }
        "#,
    );
    let consumer = TestPackage::new(
        r#"
        import lib::identity;
        entry_fn() => i32 { identity(7) }
        "#,
    );
    let program = Driver::new(
        consumer.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("lib".to_string()),
            dir: library.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with generic library extern")
    .compile(&[Ident("main".to_string())], Target::DEFAULT)
    .expect("using an extern-owned generic should compile");

    let library_path = vec![Ident("lib".to_string())];
    let library_module = program
        .modules
        .iter()
        .find_map(|(path, module)| (path == &library_path).then_some(module))
        .expect("the emitted generic instantiation keeps the library module path");
    assert!(library_module.items.iter().any(|item| {
        matches!(
            item,
            CheckedItem::FunctionDefinition(function) if function.name.as_ref() == "identity"
        )
    }));
}

#[test]
fn extern_owned_concrete_conform_is_imported_not_reemitted() {
    let library = TestPackage::new(
        r#"
        exposed spec Show { show(*self) => i32; }
        exposed struct Value { exposed n: i32; }
        meet Show for Value { show(*self) => i32 { self.n } }
        "#,
    );
    let consumer = TestPackage::new(
        r#"
        import lib::Show;
        import lib::Value;

        entry_fn() => i32 {
            value := Value { n = 7; };
            Show::show(&value)
        }
        "#,
    );
    let program = Driver::new(
        consumer.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("lib".to_string()),
            dir: library.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with library extern")
    .compile(&[Ident("main".to_string())], Target::DEFAULT)
    .expect("calling an extern-owned concrete conformance should compile");

    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(definitions, 1, "only the consumer's main should be defined");
    assert!(
        program
            .extern_functions
            .iter()
            .any(|function| matches!(function.kind, ExternFunctionKind::Conform { .. }))
    );
}

#[test]
fn blanket_conforms_require_a_package_local_spec() {
    let library = TestPackage::new("exposed spec Foreign { show(*self) => i32; }");
    let consumer = TestPackage::new(
        r#"
        import lib::Foreign;
        meet<T> Foreign for T { show(*self) => i32 { 1 } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let result = Driver::new(
        consumer.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("lib".to_string()),
            dir: library.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct consumer")
    .compile(&[Ident("main".to_string())], Target::DEFAULT);
    let errors = match result {
        Ok(_) => panic!("a blanket conforming a foreign spec must be rejected"),
        Err(errors) => errors,
    };
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::BlanketConformanceForeignSpec { .. }
    )));
}

#[test]
fn externally_owned_stdout_cannot_conform_to_externally_owned_write() {
    let core = option_core();
    let library = TestPackage::new(
        r#"
        exposed spec Write { write(*mut self, bytes: *[?]u8) => Option<usize>; }
        exposed marker Stdout {}
        meet Write for Stdout {
            write(*mut self, bytes: *[?]u8) => Option<usize> {
                Option<usize>::Some { value = <usize>bytes.length; }
            }
        }
        "#,
    );
    let consumer = TestPackage::new(
        r#"
        import lib::Stdout;
        import lib::Write;

        meet Write for Stdout {
            write(*mut self, bytes: *[?]u8) => Option<usize> {
                Option<usize>::Some { value = <usize>bytes.length; }
            }
        }
        entry_fn() => i32 { 0 }
        "#,
    );
    let mut driver = Driver::new(
        consumer.0.clone(),
        None,
        vec![
            ExternRoot {
                name: Ident("core".to_string()),
                dir: core.0.clone(),
            },
            ExternRoot {
                name: Ident("lib".to_string()),
                dir: library.0.clone(),
            },
        ],
        Target::DEFAULT,
    )
    .expect("construct driver with I/O library extern");
    let errors = match driver.compile(&[Ident("main".to_string())], Target::DEFAULT) {
        Ok(_) => panic!("a consumer must not conform two foreign I/O items"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::ConformanceOrphanViolation { .. }
        )),
        "expected an orphan violation, got {errors:#?}"
    );
}

#[test]
fn old_boolean_console_glue_signature_is_rejected() {
    let core = option_core();
    let package = TestPackage::new(
        r#"
        gap StandardOutput { write(bytes: *[?]u8) => Option<usize>; }
        glue StandardOutput {
            write(bytes: *[?]u8) => bool { true }
        }
        entry_fn() => i32 { 0 }
        "#,
    );
    let mut driver = Driver::new(
        package.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with Option core extern");
    let errors = match driver.compile(&[Ident("main".to_string())], Target::DEFAULT) {
        Ok(_) => panic!("an old console glue signature must fail"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::GlueFunctionSignatureMismatch { .. }
        )),
        "expected a glue signature mismatch, got {errors:#?}"
    );
}

#[test]
fn print_macro_requires_an_explicit_import() {
    let package = TestPackage::new("entry_fn() => i32 { println$(\"missing\"); 0 }");
    let errors = compile_errors(&package, "an unimported print macro must fail");
    assert!(errors.iter().any(|error| matches!(
        error,
        CompileError::MacroExpansion {
            error: MacroError::UnknownMacro { name },
            ..
        } if name.as_ref() == "println"
    )));
}

#[test]
fn formatting_is_not_available_from_core() {
    let core = TestPackage::new("marker CoreOnly {}");
    let consumer = TestPackage::new("entry_fn() => i32 { core::fmt::missing() }");
    let mut driver = Driver::new(
        consumer.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with core extern");
    let errors = match driver.compile(&[Ident("main".to_string())], Target::DEFAULT) {
        Ok(_) => panic!("core must not provide a formatting module"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(path))
                if path.iter().map(Ident::as_ref).eq(["core", "fmt"])
        )),
        "expected core::fmt to be unresolved, got {errors:#?}"
    );
}

#[test]
fn shared_items_are_visible_across_executable_modules() {
    let package = TestPackage::new(
        r#"
        import self::helper::fortytwo;
        entry_fn() => i32 { fortytwo() }
        "#,
    );
    package.write_child("helper", "shared fortytwo() => i32 { 42 }");
    package
        .compile()
        .expect("an executable's root and child modules share package-wide visibility");
}

#[test]
fn root_imports_are_anchored_to_the_package_root_module() {
    let package = TestPackage::new(
        r#"
        import root::helper::fortytwo;
        entry_fn() => i32 { fortytwo() }
        "#,
    );
    package.write_child("helper", "shared fortytwo() => i32 { 42 }");
    package
        .compile()
        .expect("root imports from a child should remain inside the package");
}

#[test]
fn a_bare_local_import_no_longer_resolves_relatively() {
    let package = TestPackage::new(
        r#"
        import helper::fortytwo;
        entry_fn() => i32 { fortytwo() }
        "#,
    );
    package.write_child("helper", "shared fortytwo() => i32 { 42 }");
    let errors = compile_errors(
        &package,
        "unprefixed imports no longer fall back to package-relative lookup",
    );
    assert!(has_module_resolution_error(&errors, |error| matches!(
        error,
        ResolveError::UnknownTopLevelPackage(name) if name.as_ref() == "helper"
    )));
}

#[test]
fn self_anchor_resolves_a_directory_shaped_modules_child() {
    let package = TestPackage::new(
        r#"
        import self::outer::compute;
        entry_fn() => i32 { compute() }
        "#,
    );
    package.write_nested(
        "outer/outer",
        r#"
        import self::inner::helper_value;
        exposed compute() => i32 { helper_value() }
        "#,
    );
    package.write_nested("outer/inner", "exposed helper_value() => i32 { 42 }");
    package
        .compile()
        .expect("self:: resolves against the logical module regardless of its filesystem shape");
}

#[test]
fn chained_super_resolves_across_multiple_nesting_levels() {
    let package = TestPackage::new(
        r#"
        import self::a::b::c::compute;
        entry_fn() => i32 { compute() }
        "#,
    );
    package.write_child("helper", "exposed value() => i32 { 42 }");
    package.write_nested(
        "a/b/c",
        r#"
        import super::super::super::helper::value;
        exposed compute() => i32 { value() }
        "#,
    );
    package
        .compile()
        .expect("a chained super:: removes one logical segment per occurrence");
}

#[test]
fn super_above_a_nested_modules_package_root_is_a_deterministic_error() {
    let package = TestPackage::new("entry_fn() => i32 { 0 }");
    package.write_child("leaf", "import super::super::helper::value;");
    let errors = compile_errors(
        &package,
        "super:: may not remove the importing module's own package-root segment",
    );
    assert!(has_module_resolution_error(&errors, |error| matches!(
        error,
        ResolveError::SuperAboveRoot { depth: 2, .. }
    )));
}

fn has_module_resolution_error(
    errors: &[CompileError],
    predicate: impl Fn(&ResolveError) -> bool,
) -> bool {
    errors
        .iter()
        .any(|error| matches!(error, CompileError::Resolve { error, .. } if predicate(error)))
        || has_analysis_error(
            errors,
            |kind| matches!(kind, AnalysisErrorKind::ModuleResolution(error) if predicate(error)),
        )
}

#[test]
fn local_and_extern_root_identities_cannot_collide() {
    let local = TestPackage::new("entry_fn() => i32 { 0 }");
    let dependency = TestPackage::new("exposed value() => i32 { 42 }");
    let errors = match Driver::new(
        local.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("main".to_string()),
            dir: dependency.0.clone(),
        }],
        Target::DEFAULT,
    ) {
        Ok(_) => panic!("local and extern package identities must not collide"),
        Err(errors) => errors,
    };
    assert!(matches!(
        errors.as_slice(),
        [CompileError::DuplicateModuleIdentity { name, .. }] if name.as_ref() == "main"
    ));
}

#[test]
fn spec_qualified_calls_adapt_a_non_place_receiver() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        meet Speak for Dog { speak(*self) => i32 { self.value } }
        make() => Dog { Dog { value = 3; } }
        entry_fn() => i32 {
            dog := Dog { value = 7; };
            Speak::speak(dog)
                + Speak::speak(&dog)
                + Speak::speak(Dog { value = 1; })
                + Speak::speak(make())
        }
        "#,
    );
    package
        .compile()
        .expect("a non-place spec-qualified receiver should be adapted, not rejected");
}

#[test]
fn unconstrained_conformance_parameters_are_rejected() {
    let unfixed_parameter = TestPackage::new(
        r#"
        exposed spec Bound { zero(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        struct Box<T> { exposed value: T; }
        meet<T, U: Bound> Sum for Box<T> { sum(*self) => i32 { 0 } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(
        &unfixed_parameter,
        "an unbindable parameter must be rejected",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::UnconstrainedConformanceParameter { .. }
    )));

    let generic_target = TestPackage::new(
        r#"
        exposed spec Sum { sum(*self) => i32; }
        struct Box<T> { exposed value: T; }
        meet<T> Sum for Box<T> { sum(*self) => i32 { 1 } }
        use_sum<X: Sum>(value: *X) => i32 { value.sum() }
        entry_fn() => i32 { boxed := Box<i32> { value = 1; }; use_sum(&boxed) }
        "#,
    );
    generic_target
        .compile()
        .expect("a generic target that fixes its parameter is not a blanket conform");
}

#[test]
fn blanket_conformances_materialize_and_explicit_blocks_win() {
    let package = TestPackage::new(
        r#"
        exposed spec Numeric { numeric(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        struct Number { exposed value: i32; }
        meet Numeric for Number { numeric(*self) => i32 { self.value } }
        meet<T: Numeric> Sum for T { sum(*self) => i32 { 1 } }
        meet Sum for Number { sum(*self) => i32 { 99 } }
        call<T: Sum>(value: *T) => i32 { value.sum() }
        entry_fn() => i32 { number := Number { value = 7; }; call(&number) + Sum::sum(&number) }
        "#,
    );
    package
        .compile()
        .expect("a concrete conform should supersede a matching blanket");
}

#[test]
fn a_superseded_blanket_body_is_not_type_checked() {
    let package = TestPackage::new(
        r#"
        exposed spec Numeric { numeric(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        struct Number { exposed value: i32; }
        meet Numeric for Number { numeric(*self) => i32 { self.value } }
        meet Sum for Number { sum(*self) => i32 { 7 } }
        meet<T: Numeric> Sum for T {
            sum(*self) => i32 { self.this_member_does_not_exist }
        }
        call<T: Sum>(value: *T) => i32 { value.sum() }
        entry_fn() => i32 { number := Number { value = 1; }; call(&number) }
        "#,
    );
    package
        .compile()
        .expect("a superseded blanket body must not produce diagnostics");
}

#[test]
fn a_more_specific_blanket_bound_wins() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct Number { exposed value: i32; }
        meet A for Number { a(*self) => i32 { self.value } }
        meet B for Number { b(*self) => i32 { self.value } }
        meet<T: A> Show for T { show(*self) => i32 { 1 } }
        meet<T: A + B> Show for T { show(*self) => i32 { 2 } }
        call<T: Show>(value: *T) => i32 { value.show() }
        entry_fn() => i32 { value := Number { value = 7; }; call(&value) }
        "#,
    );
    let program = package
        .compile()
        .expect("the A + B-bounded blanket should supersede the A-bounded blanket");
    let bodies = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(function) if function.name.as_ref() == "show"))
        .count();
    assert_eq!(bodies, 1, "only the winning blanket may emit a body");
}

#[test]
fn a_bounded_blanket_wins_over_an_unbounded_one() {
    let package = TestPackage::new(
        r#"
        exposed spec Numeric { zero(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct Number { exposed value: i32; }
        meet Numeric for Number { zero(*self) => i32 { 0 } }
        meet<T> Show for T { show(*self) => i32 { 1 } }
        meet<T: Numeric> Show for T { show(*self) => i32 { 2 } }
        call<T: Show>(value: *T) => i32 { value.show() }
        entry_fn() => i32 { value := Number { value = 7; }; call(&value) }
        "#,
    );
    let program = package
        .compile()
        .expect("a bounded blanket must supersede an unbounded one, not duplicate it");
    let bodies = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(function) if function.name.as_ref() == "show"))
        .count();
    assert_eq!(bodies, 1, "only the bounded blanket may emit a body");
}

#[test]
fn incomparable_blanket_bound_sets_are_ambiguous() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec C { c(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct Number { exposed value: i32; }
        meet A for Number { a(*self) => i32 { self.value } }
        meet B for Number { b(*self) => i32 { self.value } }
        meet C for Number { c(*self) => i32 { self.value } }
        meet<T: A + B> Show for T { show(*self) => i32 { 1 } }
        meet<T: A + C> Show for T { show(*self) => i32 { 2 } }
        call<T: Show>(value: *T) => i32 { value.show() }
        entry_fn() => i32 { value := Number { value = 7; }; call(&value) }
        "#,
    );
    let errors = compile_errors(
        &package,
        "neither conjunction blanket is more specific than the other",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::AmbiguousConformance { .. }
    )));
}

#[test]
fn an_explicit_conform_displaces_a_blanket_registered_before_it() {
    let package = TestPackage::new(
        r#"
        exposed spec Marker { mark(*self) => i32; }
        exposed spec Base { b(*self) => i32; }
        exposed spec Producer { make(*self) => spec Base; }
        struct Foo { exposed value: i32; }
        struct Gen { exposed value: i32; }
        meet Marker for Foo { mark(*self) => i32 { 7 } }
        meet<T: Marker> Base for T { b(*self) => i32 { 111 } }
        meet Producer for Gen { make(*self) => Foo { Foo { value = 1; } } }
        meet Base for Foo { b(*self) => i32 { 222 } }
        entry_fn() => i32 { value := Foo { value = 1; }; Base::b(&value) }
        "#,
    );
    let program = package
        .compile()
        .expect("an explicit conform must win over a blanket, whatever the registration order");
    let bodies: Vec<_> = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter_map(|item| match item {
            CheckedItem::FunctionDefinition(function) if function.name.as_ref() == "b" => {
                Some(function)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        bodies.len(),
        1,
        "the superseded blanket must not emit a `b` body as well"
    );
}

#[test]
fn unrelated_matching_blankets_are_ambiguous() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct Number { exposed value: i32; }
        meet A for Number { a(*self) => i32 { 1 } }
        meet B for Number { b(*self) => i32 { 2 } }
        meet<T: A> Show for T { show(*self) => i32 { 1 } }
        meet<T: B> Show for T { show(*self) => i32 { 2 } }
        call<T: Show>(value: *T) => i32 { value.show() }
        entry_fn() => i32 { value := Number { value = 7; }; call(&value) }
        "#,
    );
    let errors = compile_errors(
        &package,
        "unrelated blanket bounds must not pick arbitrarily",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::AmbiguousConformance { .. }
    )));
}

#[test]
fn cyclic_blanket_bounds_report_an_error_without_recursing() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        struct Number { exposed value: i32; }
        meet<T: A> B for T { b(*self) => i32 { 1 } }
        meet<T: B> A for T { a(*self) => i32 { 2 } }
        call<T: A>(value: *T) => i32 { value.a() }
        entry_fn() => i32 { value := Number { value = 7; }; call(&value) }
        "#,
    );
    let errors = compile_errors(&package, "cyclic blanket bounds must terminate");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ConformanceCycle { .. }
    )));
}

#[test]
fn generic_conform_bounds_seed_the_body_context() {
    let package = TestPackage::new(
        r#"
        exposed spec W { w(*self, value: i32) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        exposed spec QualifiedSum { qualified_sum(*self) => i32; }

        struct One { exposed value: i32; }
        struct Two { exposed value: i32; }
        meet W for One { w(*self, value: i32) => i32 { self.value + value } }
        meet W for Two { w(*self, value: i32) => i32 { self.value + value } }

        struct Buf<T> { exposed inner: *T; }
        meet<T: W> Sum for Buf<T> {
            sum(*self) => i32 { self.inner.w(1) }
        }
        meet<T: W> QualifiedSum for Buf<T> {
            qualified_sum(*self) => i32 { W::w(self.inner, 1) }
        }

        use_sum<T: Sum>(value: *T) => i32 { value.sum() }
        use_qualified_sum<T: QualifiedSum>(value: *T) => i32 { value.qualified_sum() }
        entry_fn() => i32 {
            one := One { value = 1; };
            two := Two { value = 2; };
            first := Buf<One> { inner = &one; };
            second := Buf<Two> { inner = &two; };
            use_sum(&first) + use_qualified_sum(&first) + use_sum(&second)
        }
        "#,
    );
    package
        .compile()
        .expect("a conform generic bound must both validate and seed its body context");
}

#[test]
fn generic_conform_bounds_reject_unsatisfied_conformance_at_the_declaration() {
    let source = r#"
        exposed spec W { w(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct NotW { exposed value: i32; }
        struct Buf<T> { exposed inner: *T; }
        meet<T: W> Show for Buf<T> { show(*self) => i32 { 1 } }

        entry_fn() => i32 {
            value := NotW { value = 0; };
            buf := Buf<NotW> { inner = &value; };
            Show::show(&buf)
        }
        "#;
    let package = TestPackage::new(source);
    let errors = compile_errors(
        &package,
        "an unsatisfied conform generic bound must not produce a conformance or vtable",
    );
    let expected_start = source
        .find("meet<T: W> Show for Buf<T>")
        .expect("the declaration is present");
    let error = errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors.iter(),
            _ => [].iter(),
        })
        .find(|error| {
            matches!(
                error.kind,
                AnalysisErrorKind::ModuleResolution(
                    omega_analyzer::resolver::ResolveError::SpecNotImplemented { .. }
                )
            )
        })
        .expect("the conform bound failure is reported as SpecNotImplemented");
    assert_eq!(error.span.start, expected_start);
}

#[test]
fn an_unrelated_spec_query_does_not_report_a_foreign_template_bound() {
    let package = TestPackage::new(
        r#"
        exposed spec W { w(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct NotW { exposed value: i32; }
        struct Buf<T> { exposed inner: *T; }
        meet<T: W> Show for Buf<T> { show(*self) => i32 { 1 } }
        as_w(value: *Buf<NotW>) => *spec W { value }
        entry_fn() => i32 {
            value := NotW { value = 0; };
            buf := Buf<NotW> { inner = &value; };
            as_w(&buf).w()
        }
        "#,
    );
    let errors = compile_errors(&package, "the failing cast must still be rejected");
    assert!(
        !has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::ModuleResolution(
                omega_analyzer::resolver::ResolveError::SpecNotImplemented { .. }
            )
        )),
        "a query for an unrelated spec must not report the Show template's bound failure"
    );
}

#[test]
fn generic_conform_bounds_expand_conjunctions() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32 { 2 } }
        exposed spec Sum { sum(*self) => i32; }
        struct Value { exposed value: i32; }
        meet A for Value { a(*self) => i32 { self.value } }
        meet B for Value { }

        struct Buf<T> { exposed inner: *T; }
        meet<T: A + B> Sum for Buf<T> {
            sum(*self) => i32 { self.inner.a() + self.inner.b() }
        }
        use_sum<T: Sum>(value: *T) => i32 { value.sum() }
        entry_fn() => i32 {
            value := Value { value = 1; };
            buf := Buf<Value> { inner = &value; };
            use_sum(&buf)
        }
        "#,
    );
    package
        .compile()
        .expect("a conform generic conjunction bound must reach its member conformances");
}

#[test]
fn an_unbounded_generic_conform_gains_no_bound_context() {
    let package = TestPackage::new(
        r#"
        exposed spec Secret { secret(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct Value { exposed value: i32; }
        meet Secret for Value { secret(*self) => i32 { self.value } }

        struct Box<T> { exposed inner: *T; }
        meet<T> Show for Box<T> {
            show(*self) => i32 { self.inner.secret() }
        }
        use_show<T: Show>(value: *T) => i32 { value.show() }
        entry_fn() => i32 {
            value := Value { value = 1; };
            boxed := Box<Value> { inner = &value; };
            use_show(&boxed)
        }
        "#,
    );
    let errors = compile_errors(
        &package,
        "an unbounded conform must not gain methods from its concrete argument",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn an_inherent_method_body_cannot_reach_a_conformance_method() {
    let package = TestPackage::new(
        r#"
        exposed spec Secret { secret(*self) => i32; }
        struct Dog {
            exposed value: i32;
            exposed leak(*self) => i32 { self.secret() }
        }
        meet Secret for Dog { secret(*self) => i32 { 99 } }
        entry_fn() => i32 { dog := Dog { value = 1; }; dog.leak() }
        "#,
    );
    let errors = compile_errors(
        &package,
        "an inherent body must not see a conforming method",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn distinct_generic_spec_conformances_emit_distinct_bodies() {
    let package = TestPackage::new(
        r#"
        exposed spec Consume<T> { consume(*self, value: T) => i32; }
        struct Multi { exposed base: i32; }
        meet Consume<i32> for Multi {
            consume(*self, value: i32) => i32 { self.base + value }
        }
        meet Consume<u8> for Multi {
            consume(*self, value: u8) => i32 { self.base + <i32>value }
        }
        entry_fn() => i32 {
            value := Multi { base = 1; };
            Consume<i32>::consume(&value, 2) + Consume<u8>::consume(&value, 3u8)
        }
        "#,
    );
    let program = package
        .compile()
        .expect("both generic spec conformances should compile");
    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(
        definitions, 3,
        "main and both conform bodies must be emitted"
    );
}

#[test]
fn a_bound_on_a_conjunction_reaches_its_members_conformances() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32 { 2 } }
        struct Foo { exposed v: i32; }
        meet A for Foo { a(*self) => i32 { 1 } }
        meet B for Foo { }

        use_conjunction<T: A + B>(x: *T) => i32 { x.a() + x.b() }
        entry_fn() => i32 { f := Foo { v = 0; }; use_conjunction(&f) }
        "#,
    );
    package
        .compile()
        .expect("a conjunction bound must resolve through its members' conformances");
}

#[test]
fn an_unbounded_spec_is_still_out_of_scope_under_another_bound() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        exposed spec Secret { secret(*self) => i32; }
        struct Dog { exposed id: i32; }
        meet Speak for Dog { speak(*self) => i32 { self.id } }
        meet Secret for Dog { secret(*self) => i32 { 999 } }

        leak<T: Speak>(x: *T) => i32 { x.secret() }
        entry_fn() => i32 { d := Dog { id = 7; }; leak(&d) }
        "#,
    );
    let errors = compile_errors(&package, "an unbounded spec's method must not be in scope");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn slice_conformances_are_callable_not_merely_declarable() {
    for (generics, target) in [("", "[]u8"), ("<T>", "[]T")] {
        let package = TestPackage::new(&format!(
            r#"
            exposed spec Show {{ show(*self) => i32; }}
            meet{generics} Show for {target} {{ show(*self) => i32 {{ self.length }} }}
            entry_fn() => i32 {{
                mut a: [2]u8;
                s := &a[0..];
                Show::show(s)
            }}
            "#,
        ));
        package
            .compile()
            .unwrap_or_else(|_| panic!("a `{target}` conformance must be callable"));
    }
}

#[test]
fn inferred_arrays_slices_and_unsized_array_pointers_have_distinct_spellings() {
    let package = TestPackage::new(
        r#"
        takes_slice(value: *[]i32) => i32 { value.length }
        takes_unsized(value: *[?]i32) => i32 { value[1] }
        entry_fn() => i32 {
            inferred: []i32 = [10, 20, 30];
            unsized := <*[?]i32>&inferred;
            slice := &inferred[0..];
            takes_slice(slice) + takes_unsized(unsized)
        }
        "#,
    );
    package
        .compile()
        .expect("the new array and slice spellings should resolve to their distinct shapes");
}

#[test]
fn an_unmatchable_generic_conform_target_is_rejected_at_its_declaration() {
    let package = TestPackage::new(
        r#"
        exposed spec Show { show(*self) => i32; }
        meet<T> Show for *T { show(*self) => i32 { 1 } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "a pointer conform target must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ConformTargetNotAType
    )));
}

#[test]
fn a_variadic_spec_function_is_rejected_at_its_declaration() {
    let package = TestPackage::new(
        r#"
        exposed spec Fmt { emit(*self, ...) => i32; }
        entry_fn() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "a variadic spec function must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::VariadicSpecFunctionUnsatisfiable { .. }
    )));
}

#[test]
fn a_spec_return_type_on_a_method_is_rejected_not_inferred() {
    let package = TestPackage::new(
        r#"
        exposed spec Countable { count(*self) => i32; }
        struct Wrap { exposed n: i32; }
        meet Countable for Wrap { count(*self) => i32 { self.n } }
        struct Zoo {
            exposed n: i32;
            exposed helper(*self) => i32 { 5 }
            exposed make(*self) => spec Countable { Wrap { n = self.helper(); } }
        }
        entry_fn() => i32 { z := Zoo { n = 1; }; Countable::count(&z.make()) }
        "#,
    );
    let errors = compile_errors(&package, "a `spec T`-returning method must be rejected");
    assert!(
        !has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::NoSuchField { .. }
        )),
        "must not fail by observing a partially-populated cell"
    );
}

#[test]
fn a_spec_return_type_on_a_free_function_is_rejected_not_inferred() {
    let package = TestPackage::new(
        r#"
        exposed spec Animal { speak(*self) => i32; }
        struct Dog { exposed v: i32; }
        meet Animal for Dog { speak(*self) => i32 { self.v } }
        make() => spec Animal { Dog { v = 1; } }
        entry_fn() => i32 { Animal::speak(&make()) }
        "#,
    );
    let errors = compile_errors(
        &package,
        "a `spec T`-returning free function must be rejected",
    );
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::UnresolvedType(TypeResolutionError::SpecStaticNotAllowedHere(_))
        )),
        "the definition-site `spec T` return must be rejected as SpecStaticNotAllowedHere"
    );
}

#[test]
fn a_mismatched_for_loop_element_annotation_reports_what_is_available() {
    let package = TestPackage::new(
        r#"
        exposed struct BagIter { exposed i: i32; }
        meet Iterator<u8> for BagIter { next(*mut self) => Option<u8> { Option<u8>::None } }
        exposed struct Bag { exposed n: i32; }
        meet ToIterator<u8> for Bag { to_iterator(*self) => BagIter { BagIter { i = 0; } } }
        entry_fn() => i32 { b := Bag { n = 0; }; for x : u64 in b { } 0 }
        "#,
    );
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/core");
    let errors = match Driver::new(
        package.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core,
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with core extern")
    .compile(&[Ident("main".to_string())], Target::DEFAULT)
    {
        Ok(_) => panic!("a mismatched element annotation must be rejected"),
        Err(errors) => errors,
    };
    assert!(has_analysis_error(&errors, |kind| match kind {
        AnalysisErrorKind::ForLoopElementTypeMismatch { available, .. } => !available.is_empty(),
        _ => false,
    }));
}

#[test]
fn primitive_method_symbols_stay_within_the_mangling_charset() {
    let package = TestPackage::new(
        r#"
        primitive str { exposed width(*self) => i32 { self.size } }
        entry_fn() => i32 { 0 }
        "#,
    );
    let root = package.0.clone();
    let program = Driver::new(
        root,
        Some(Ident("core".to_string())),
        Vec::new(),
        Target::DEFAULT,
    )
    .expect("construct driver")
    .compile(&[Ident("core".to_string())], Target::DEFAULT);
    let program = match program {
        Ok(program) => program,
        Err(errors) => panic!("core-shaped primitive package must compile: {errors:?}"),
    };
    let names: Vec<String> = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter_map(|item| match item {
            CheckedItem::FunctionDefinition(f) => Some(f.name.as_ref().to_string()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|name| name == "width"));
}

#[test]
fn a_package_root_with_no_modules_is_a_reportable_error() {
    let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "omega_empty_pkg_{}_{}",
        std::process::id(),
        sequence
    ));
    let nested = root.join(root.file_name().expect("temp dir has a name"));
    fs::create_dir_all(&nested).expect("create nested package");
    let own = format!("{}.omg", root.file_name().unwrap().to_str().unwrap());
    fs::write(nested.join(&own), "exposed helper() => i32 { 7 }\n").expect("write inner module");

    let result = Driver::new(root.clone(), None, Vec::new(), Target::DEFAULT)
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], Target::DEFAULT);
    let _ = fs::remove_dir_all(&root);

    let errors = result
        .err()
        .expect("an empty package root must not compile");
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, CompileError::EmptyPackage { .. })),
        "expected EmptyPackage, got {errors:?}"
    );
}

fn compile_as_core(core_source: &str) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
    let core = TestPackage::new(core_source);
    let local = TestPackage::new("entry_fn() => i32 { 0 }");
    let result = Driver::new(
        local.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
        Target::DEFAULT,
    )
    .expect("construct driver with core extern")
    .compile(&[Ident("main".to_string())], Target::DEFAULT);
    drop(core);
    result
}

#[test]
fn an_empty_primitive_block_is_a_valid_declaration() {
    compile_as_core("primitive char { }\nprimitive bool { }")
        .expect("an empty primitive block must be accepted");
}

#[test]
fn void_and_never_have_declaration_sites() {
    compile_as_core("primitive void { }\nprimitive never { }")
        .expect("`void`/`never` must be declarable");
}

#[test]
fn a_struct_is_not_a_primitive_target() {
    let Err(errors) = compile_as_core("struct S { x: i32; }\nprimitive S { }") else {
        panic!("a struct must not be a primitive target");
    };
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::PrimitiveTargetNotAllowed { .. }
    )));
}

#[test]
fn void_is_declarable_but_not_conformable() {
    let Err(errors) = compile_as_core(
        "exposed spec Show { show(*self) => i32; }\nmeet Show for void { show(*self) => i32 { 0 } }",
    ) else {
        panic!("`void` must not be conformable");
    };
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ConformTargetNotAType
    )));
}

#[test]
fn a_genuine_conformance_cycle_is_rejected() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        struct S { exposed v: i32; }
        meet<T: B> A for T { a(*self) => i32 { 1 } }
        meet<T: A> B for T { b(*self) => i32 { 2 } }
        use_a<T: A>(t: T) => i32 { t.a() }
        entry_fn() => i32 { use_a(S { v = 0; }) }
        "#,
    );
    let errors = compile_errors(&package, "a genuine cycle must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ConformanceCycle { .. }
    )));
}

#[test]
fn a_blanket_chain_compiles_in_either_declaration_order() {
    for swap in [false, true] {
        let package = TestPackage::new(&format!(
            r#"
            exposed spec A {{ a(*self) => i32; }}
            exposed spec B {{ b(*self) => i32; }}
            exposed spec C {{ c(*self) => i32; }}
            struct S {{ exposed v: i32; }}
            meet A for S {{ a(*self) => i32 {{ 1 }} }}
            {}
            {}
            use_c<T: C>(t: T) => i32 {{ t.c() }}
            entry_fn() => i32 {{ use_c(S {{ v = 0; }}) }}
            "#,
            if swap {
                "meet<T: B> C for T { c(*self) => i32 { self.b() + 1 } }"
            } else {
                "meet<T: A> B for T { b(*self) => i32 { self.a() + 1 } }"
            },
            if swap {
                "meet<T: A> B for T { b(*self) => i32 { self.a() + 1 } }"
            } else {
                "meet<T: B> C for T { c(*self) => i32 { self.b() + 1 } }"
            },
        ));
        package
            .compile()
            .expect("the blanket chain compiles in either declaration order");
    }
}

#[test]
fn a_blanket_chain_with_a_concrete_middle_link_still_works() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec C { c(*self) => i32; }
        struct S { exposed v: i32; }
        meet A for S { a(*self) => i32 { 1 } }
        meet B for S { b(*self) => i32 { self.v + 1 } }
        meet<T: B> C for T { c(*self) => i32 { self.b() + 1 } }
        use_c<T: C>(t: T) => i32 { t.c() }
        entry_fn() => i32 { use_c(S { v = 0; }) }
        "#,
    );
    package
        .compile()
        .expect("a blanket chain with a concrete middle link compiles");
}

#[test]
fn a_fourth_blanket_bounded_on_the_middle_spec_compiles() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec C { c(*self) => i32; }
        exposed spec X { x(*self) => i32; }
        struct S { exposed v: i32; }
        meet A for S { a(*self) => i32 { 1 } }
        meet<T: A> B for T { b(*self) => i32 { self.a() + 1 } }
        meet<T: B> C for T { c(*self) => i32 { self.b() + 1 } }
        meet<T: B> X for T { x(*self) => i32 { self.b() + 10 } }
        use_c<T: C>(t: T) => i32 { t.c() }
        use_x<T: X>(t: T) => i32 { t.x() }
        entry_fn() => i32 { use_c(S { v = 0; }) + use_x(S { v = 0; }) }
        "#,
    );
    package
        .compile()
        .expect("an unrelated fourth blanket does not disturb the chain");
}

#[test]
fn a_template_whose_spec_does_not_resolve_still_reports_not_a_spec() {
    let package = TestPackage::new(
        r#"
        struct Plain { exposed v: i32; }
        struct Wrapper<T> { exposed v: T; }
        meet<T> Plain for Wrapper<T> { }
        entry_fn() => i32 { w := Wrapper { v = 1; }; w.nothing_here() }
        "#,
    );
    let errors = compile_errors(&package, "the non-spec conform target must be reported");
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name))
                if name.as_ref() == "Plain"
        )),
        "expected NotASpec naming the template's non-spec target"
    );
}

#[test]
fn a_blanket_conform_declared_for_both_member_orderings_is_a_duplicate() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec X { x(*self) => i32; }
        struct S { exposed v: i32; }
        meet A for S { a(*self) => i32 { 0 } }
        meet B for S { b(*self) => i32 { 0 } }
        meet<T: A + B> X for T { x(*self) => i32 { 1 } }
        meet<T: B + A> X for T { x(*self) => i32 { 2 } }
        use_x<T: X>(t: T) => i32 { t.x() }
        entry_fn() => i32 { use_x(S { v = 0; }) }
        "#,
    );
    let errors = compile_errors(
        &package,
        "A + B and B + A name the same bound, so both blankets collide",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::DuplicateConformance { .. }
    )));
}

#[test]
fn a_blanket_declared_in_one_member_order_is_reachable_through_the_other() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }
        exposed spec X { x(*self) => i32; }
        struct S { exposed v: i32; }
        meet A for S { a(*self) => i32 { 1 } }
        meet B for S { b(*self) => i32 { 2 } }
        meet<T: A + B> X for T { x(*self) => i32 { 10 } }
        use_reordered<T: B + A>(t: T) => i32 { t.x() }
        entry_fn() => i32 { use_reordered(S { v = 0; }) }
        "#,
    );
    package
        .compile()
        .expect("a blanket declared under one member order must be reachable through the other");
}

#[test]
fn a_generic_conjunction_bound_ignores_declaration_order() {
    let package = TestPackage::new(
        r#"
        exposed spec Iter<T> { next(*self) => i32; }
        exposed spec Eq { equals(*self, other: *Self) => bool; }
        exposed spec X { x(*self) => i32; }
        struct S { exposed v: i32; }
        meet Iter<i32> for S { next(*self) => i32 { self.v } }
        meet Eq for S { equals(*self, other: *S) => bool { false } }
        meet<T: Iter<i32> + Eq> X for T { x(*self) => i32 { 1 } }
        meet<T: Eq + Iter<i32>> X for T { x(*self) => i32 { 2 } }
        use_x<T: X>(t: T) => i32 { t.x() }
        entry_fn() => i32 { use_x(S { v = 0; }) }
        "#,
    );
    let errors = compile_errors(
        &package,
        "reordering a generic conjunction's members must not change the bound",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::DuplicateConformance { .. }
    )));
}

#[test]
fn a_return_type_only_generic_is_inferred_from_the_expected_type() {
    let package = TestPackage::new(
        r#"
        exposed spec Bounded { min() => Self; }
        struct Small { exposed v: i32; }
        struct Big { exposed v: i64; }
        meet Bounded for Small { min() => Self { Small { v = 1; } } }
        meet Bounded for Big { min() => Self { Big { v = 2; } } }

        lowest<T: Bounded>() => T { T::min() }
        take_small(x: Small) => i32 { x.v }
        take_big(x: Big) => i64 { x.v }
        tail_return() => Small { lowest() }
        explicit_return() => Small { return lowest(); }
        branch(cond: bool) => Small { if cond { lowest() } else { Small { v = 9; } } }

        entry_fn() => i64 {
            a : Small = lowest();
            b : Big = lowest();
            c := take_small(tail_return());
            d := take_small(explicit_return());
            e := take_small(branch(true));
            f := take_big(lowest());
            <i64>a.v + b.v + <i64>c + <i64>d + <i64>e + f
        }
        "#,
    );
    package
        .compile()
        .expect("return-type-only generic infers from expected");
}

#[test]
fn a_generic_static_infers_owner_generics_from_the_expected_type() {
    let package = TestPackage::new(
        r#"
        struct BoxSelf<T> { exposed v: T; exposed empty() => Self { BoxSelf { v = 0; } } }
        struct BoxOut<T> { exposed v: T; exposed empty() => BoxOut<T> { BoxOut { v = 0; } } }
        entry_fn() => i32 {
            a : BoxSelf<i32> = BoxSelf::empty();
            b : BoxOut<i64> = BoxOut::empty();
            a.v + <i32>b.v
        }
        "#,
    );
    package
        .compile()
        .expect("generic static infers owner generics from expected");
}

#[test]
fn the_expected_type_seed_adapts_untyped_literal_arguments() {
    let package = TestPackage::new(
        r#"
        identity<T>(x: T) => T { x }
        entry_fn() => i64 { y : i64 = identity(5); y }
        "#,
    );
    package
        .compile()
        .expect("expected type adapts the literal argument");
}

#[test]
fn an_argument_conflicting_with_the_expected_seed_is_rejected() {
    let package = TestPackage::new(
        r#"
        struct Small { exposed v: i32; }
        identity<T>(x: T) => T { x }
        entry_fn() => i64 { y : i64 = identity(Small { v = 1; }); y }
        "#,
    );
    let errors = compile_errors(&package, "the seed/argument conflict must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ArgumentTypeMismatch { .. }
    )));
}

#[test]
fn a_mut_self_call_on_a_temporary_reports_mutate_temporary() {
    let package = TestPackage::new(
        r#"
        exposed spec Bumpable { bump(*mut self) => void; }
        struct Bump { exposed n: i32; }
        meet Bumpable for Bump { bump(*mut self) => void { self.n = self.n + 1; } }
        make() => Bump { Bump { n = 0; } }
        entry_fn() => i32 { Bumpable::bump(make()); 0 }
        "#,
    );
    let errors = compile_errors(&package, "an rvalue receiver must be rejected");
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::MutateTemporary
        )),
        "expected MutateTemporary, not NotMutablePointer"
    );
}

#[test]
fn a_projected_write_through_a_temporary_reports_mutate_temporary() {
    let package = TestPackage::new(
        r#"
        struct Bump { exposed n: i32; }
        make() => Bump { Bump { n = 0; } }
        entry_fn() => i32 { make().n = 5; 0 }
        "#,
    );
    let errors = compile_errors(&package, "a write into an rvalue must be rejected");
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::MutateTemporary
        )),
        "expected MutateTemporary for a projected write through a temporary"
    );
}

#[test]
fn a_thin_pointer_generic_against_a_fat_pointer_teaches_the_rule() {
    let package = TestPackage::new(
        r#"
        exposed spec Show { show(*self) => i32; }
        use_it<T: Show>(x: *T) => i32 { 1 }
        entry_fn() => i32 {
            arr := [1, 2, 3];
            slice := &arr[0..<3];
            use_it(slice)
        }
        "#,
    );
    let errors = compile_errors(
        &package,
        "the fat-pointer inference failure must be reported",
    );
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::GenericParamFromFatPointer { .. }
        )),
        "expected the dedicated fat-pointer diagnostic"
    );
}

#[test]
fn a_by_value_generic_still_binds_a_slice() {
    let package = TestPackage::new(
        r#"
        exposed spec Show { show(*self) => i32; }
        meet<T> Show for []T { show(*self) => i32 { 1 } }
        use_it<T: Show>(s: T) => i32 { 1 }
        entry_fn() => i32 {
            arr := [1, 2, 3];
            slice := &arr[0..<3];
            use_it(slice)
        }
        "#,
    );
    package
        .compile()
        .expect("the by-value form binds the slice type parameter");
}

#[test]
fn a_32_bit_target_sizes_usize_at_four_bytes() {
    let target = Target {
        arch: omega_analyzer::Arch::Riscv32,
        os: omega_analyzer::Os::None,
    };
    let package = TestPackage::new(
        r#"
        width := comp sizeof<usize>;
        width_isize := comp sizeof<isize>;
        ptr_width := comp sizeof<*u8>;
        entry_fn() => i32 { 0 }
        "#,
    );
    let program = Driver::new(package.0.clone(), None, Vec::new(), target)
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], target)
        .expect("compiles for riscv32-none");
    let width_of = |name: &str| -> Option<ConstValue> {
        program
            .modules
            .iter()
            .flat_map(|(_, module)| module.items.iter())
            .find_map(|item| match item {
                omega_analyzer::checked::CheckedItem::Declaration(decl)
                    if decl.ident.as_ref() == name =>
                {
                    Some(
                        decl.initial_value
                            .clone()
                            .expect("a `comp` global carries its value"),
                    )
                }
                _ => None,
            })
    };
    for name in ["width", "width_isize", "ptr_width"] {
        assert_eq!(
            width_of(name),
            Some(ConstValue::Number(NumberValue::Unsigned(4))),
            "{name} must be 4 on a 32-bit target"
        );
    }
}

#[test]
fn a_usize_literal_above_u32_max_is_rejected_on_a_32_bit_target() {
    let source = r#"
        n : usize = 5000000000;
        entry_fn() => i32 { 0 }
        "#;
    let target32 = Target {
        arch: omega_analyzer::Arch::Riscv32,
        os: omega_analyzer::Os::None,
    };
    let package32 = TestPackage::new(source);
    let errors32 = match Driver::new(package32.0.clone(), None, Vec::new(), target32)
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], target32)
    {
        Ok(_) => panic!("the out-of-range usize literal must be rejected on a 32-bit target"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors32, |kind| matches!(
            kind,
            AnalysisErrorKind::NumberLiteralOutOfRange { .. }
        )),
        "expected NumberLiteralOutOfRange on the 32-bit target"
    );

    let package64 = TestPackage::new(source);
    package64
        .compile()
        .expect("the same literal fits a 64-bit usize");
}

#[test]
fn lowered_mir_carries_symbols_and_linkage() {
    let package = TestPackage::new(
        r#"
        add<T>(a: T, b: T) => T { a }
        main() => void { add(1, 2); }
        "#,
    );
    let program = Driver::new(package.0.clone(), None, Vec::new(), Target::DEFAULT)
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
        .expect("compiles");
    let mir = omega_mir::lower_program(program.modules, &program.entry);
    let functions: Vec<&omega_mir::MirFunctionDef> = mir
        .iter()
        .flat_map(|(_, module)| module.items.iter())
        .filter_map(|item| match item {
            omega_mir::MirItem::FunctionDefinition(f) => Some(f),
            _ => None,
        })
        .collect();

    let main = functions
        .iter()
        .find(|f| f.name.as_ref() == "main")
        .expect("the entry function is present");
    assert_eq!(main.symbol, "_omg_main");
    assert_eq!(main.linkage, omega_mir::MirLinkage::Export);

    let add = functions
        .iter()
        .find(|f| f.name.as_ref() == "add")
        .expect("the generic instantiation is present");
    assert_eq!(add.linkage, omega_mir::MirLinkage::Weak);
    assert!(
        add.symbol.starts_with("_omg_"),
        "an ordinary function's symbol is mangled: {}",
        add.symbol
    );
}

fn error_texts(
    source: &str,
    errors: &[CompileError],
    predicate: impl Fn(&AnalysisErrorKind) -> bool,
) -> Vec<String> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors.clone(),
            _ => Vec::new(),
        })
        .filter(|error| predicate(&error.kind))
        .map(|error| source[error.span.start..error.span.end].to_string())
        .collect()
}

#[test]
fn a_duplicate_member_underlines_only_its_name() {
    for (source, name) in [
        (
            "struct Holder {\n    field: i32;\n    field: i32;\n}\nentry_fn() => i32 { 0 }\n",
            "field",
        ),
        (
            "struct Holder {\n    v: i32;\n\n    method(*self) => i32 { 1 }\n    method(*self) => i32 { 2 }\n}\nentry_fn() => i32 { 0 }\n",
            "method",
        ),
    ] {
        let package = TestPackage::new(source);
        let errors = compile_errors(&package, "duplicate members must be rejected");
        let texts = error_texts(source, &errors, |kind| {
            matches!(kind, AnalysisErrorKind::Redeclaration { .. })
        });
        assert_eq!(
            texts,
            [name],
            "a duplicate member's label must cover the name only"
        );
        for error in errors.iter() {
            let CompileError::Analysis { errors, .. } = error else {
                continue;
            };
            for error in errors {
                let AnalysisErrorKind::Redeclaration { previous, .. } = &error.kind else {
                    continue;
                };
                let previous = previous.expect("a duplicate always has a first declaration");
                assert_eq!(
                    &source[previous.start..previous.end],
                    name,
                    "the `first declared here` label must cover the name only"
                );
            }
        }
    }
}

#[test]
fn a_return_type_mismatch_underlines_the_declared_type() {
    let source = "\
sum(a: i32, b: i32) => i32 {
    a;
    b;
}
struct Holder {
    v: i32;

    get(*self) => *mut i32 {
        self.v;
    }
}
entry_fn() => i32 { 0 }
";
    let package = TestPackage::new(source);
    let errors = compile_errors(&package, "both return-type mismatches must be rejected");
    let texts = error_texts(source, &errors, |kind| {
        matches!(kind, AnalysisErrorKind::ReturnTypeMismatch { .. })
    });
    assert_eq!(
        texts,
        ["i32", "*mut i32"],
        "the label must cover the whole declared return type and nothing else"
    );
}

#[test]
fn a_duplicate_spec_function_underlines_only_its_name() {
    let source = "\
spec Sp {
    m(*self) => i32;
    m(*self) => i32;
}
entry_fn() => i32 { 0 }
";
    let package = TestPackage::new(source);
    let errors = compile_errors(&package, "a duplicate spec function must be rejected");
    let texts = error_texts(source, &errors, |kind| {
        matches!(kind, AnalysisErrorKind::Redeclaration { .. })
    });
    assert_eq!(texts, ["m"]);
}
