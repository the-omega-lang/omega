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
