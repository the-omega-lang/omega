use omega_analyzer::checked::{CheckedItem, ExternFunctionKind};
use omega_analyzer::error::AnalysisErrorKind;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        Self::with_file("main.omg", source)
    }

    fn with_file(file: &str, source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "omega_compose_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir(&root).expect("create test package");
        fs::write(root.join(file), source).expect("write test module");
        Self(root)
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::new())
            .expect("construct driver")
            .compile(&[Ident("main".to_string())])
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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

#[test]
fn bound_and_spec_qualified_dispatch_compile() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }

        call_bound<T: Speak>(value: *T) => i32 { value.speak() }
        main() => i32 {
            dog := Dog { value = 7; };
            call_bound(&dog) + Speak::speak(&dog)
        }
        "#,
    );
    package
        .compile()
        .expect("both composition call forms should compile");
}

#[test]
fn composed_instance_method_is_not_in_concrete_scope() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        main() => i32 { dog := Dog { value = 7; }; dog.speak() }
        "#,
    );
    let errors = compile_errors(&package, "concrete instance syntax must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn duplicate_and_extra_compositions_are_rejected() {
    let duplicate = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&duplicate, "duplicate composition must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::DuplicateCompose { .. }
    )));

    let extra = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak {
            speak(*self) => i32 { self.value }
            extra(*self) => i32 { 0 }
        }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&extra, "extra compose functions must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ComposeExtraFunction { .. }
    )));
}

#[test]
fn primitive_blocks_are_core_only() {
    let package = TestPackage::new(
        r#"
        primitive i32 { exposed identity(*self) => i32 { *self } }
        main() => i32 { 0 }
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
    let core = TestPackage::with_file(
        "core.omg",
        "primitive i32 { exposed identity(*self) => i32 { *self } }",
    );
    let local = TestPackage::new("main() => i32 { 7i32.identity() }");
    let mut driver = Driver::new(
        local.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
    )
    .expect("construct driver with core extern");
    let program = driver
        .compile(&[Ident("main".to_string())])
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

/// `Spec::method(receiver, ...)` must adapt `receiver` to the declared self
/// mode whatever *shape* the argument has -- a literal and a struct
/// expression are not places, and before this was fixed they reached a
/// `*self` parameter unadapted and failed to type-check. The print macros
/// expand to exactly this form (`Display::fmt($args, ...)`) over arbitrary
/// argument expressions, so every one of these shapes is load-bearing.
#[test]
fn spec_qualified_calls_adapt_a_non_place_receiver() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        make() => Dog { Dog { value = 3; } }
        main() => i32 {
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

/// Blanket composes are deliberately out of scope, and must say so rather
/// than being silently dropped: `match_compose_target` can never bind a
/// target that is itself a parameter, so without the check the only
/// diagnostic anyone saw was an unrelated `SpecNotImplemented` at a use
/// site -- or, for an unused compose, nothing at all.
#[test]
fn blanket_composes_are_rejected_with_their_own_diagnostic() {
    let bare_target = TestPackage::new(
        r#"
        exposed spec Numeric { zero(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        compose<T: Numeric> T : Sum { sum(*self) => i32 { 0 } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&bare_target, "a blanket compose must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::BlanketComposeNotYetSupported { .. }
    )));

    // A parameter that the target never mentions is equally unbindable.
    let unfixed_parameter = TestPackage::new(
        r#"
        exposed spec Bound { zero(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        struct Box<T> { exposed value: T; }
        compose<T, U: Bound> Box<T> : Sum { sum(*self) => i32 { 0 } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&unfixed_parameter, "an unbindable parameter must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::BlanketComposeNotYetSupported { .. }
    )));

    // ...while a target that does fix its parameter stays fully supported.
    let generic_target = TestPackage::new(
        r#"
        exposed spec Sum { sum(*self) => i32; }
        struct Box<T> { exposed value: T; }
        compose<T> Box<T> : Sum { sum(*self) => i32 { 1 } }
        use_sum<X: Sum>(value: *X) => i32 { value.sum() }
        main() => i32 { boxed := Box<i32> { value = 1; }; use_sum(&boxed) }
        "#,
    );
    generic_target
        .compile()
        .expect("a generic target that fixes its parameter is not a blanket compose");
}

/// A type's *inherent* method body is not a compose body, so a spec
/// composed onto that type is not in its scope. Without this, every method
/// a type declares could reach every method any package ever composed onto
/// it -- exactly the incoherence resolving composed methods through their
/// spec exists to prevent.
#[test]
fn an_inherent_method_body_cannot_reach_a_composed_method() {
    let package = TestPackage::new(
        r#"
        exposed spec Secret { secret(*self) => i32; }
        struct Dog {
            exposed value: i32;
            exposed leak(*self) => i32 { self.secret() }
        }
        compose Dog : Secret { secret(*self) => i32 { 99 } }
        main() => i32 { dog := Dog { value = 1; }; dog.leak() }
        "#,
    );
    let errors = compile_errors(&package, "an inherent body must not see a composed method");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn distinct_generic_spec_compositions_emit_distinct_bodies() {
    let package = TestPackage::new(
        r#"
        exposed spec Consume<T> { consume(*self, value: T) => i32; }
        struct Multi { exposed base: i32; }
        compose Multi : Consume<i32> {
            consume(*self, value: i32) => i32 { self.base + value }
        }
        compose Multi : Consume<u8> {
            consume(*self, value: u8) => i32 { self.base + <i32>value }
        }
        main() => i32 {
            value := Multi { base = 1; };
            Consume<i32>::consume(&value, 2) + Consume<u8>::consume(&value, 3u8)
        }
        "#,
    );
    let program = package
        .compile()
        .expect("both generic spec compositions should compile");
    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(
        definitions, 3,
        "main and both compose bodies must be emitted"
    );
}
