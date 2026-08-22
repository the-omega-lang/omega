use omega_analyzer::Target;
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
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_entry_point_test_{}_{}",
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

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::<ExternRoot>::new(), Target::DEFAULT)
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

fn compile_errors(package: &TestPackage, message: &str) -> Vec<CompileError> {
    match package.compile() {
        Ok(_) => panic!("{message}"),
        Err(errors) => errors,
    }
}

fn has_invalid_main_signature(errors: &[CompileError]) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors
            .iter()
            .any(|error| matches!(error.kind, AnalysisErrorKind::InvalidMainSignature)),
        _ => false,
    })
}

#[test]
fn void_main_is_accepted() {
    let package = TestPackage::new("main() => void { }");
    package.compile().expect("`main() => void` must be accepted");
}

#[test]
fn never_main_is_accepted() {
    let package = TestPackage::new(
        r#"
        shared extern exit : (code: i32) => never;
        main() => never { exit(0); }
        "#,
    );
    package
        .compile()
        .expect("`main() => never` must be accepted");
}

#[test]
fn main_with_a_parameter_is_rejected() {
    let package = TestPackage::new("main(argc: i32) => void { }");
    let errors = compile_errors(&package, "a parameterized `main` must be rejected");
    assert!(
        has_invalid_main_signature(&errors),
        "expected InvalidMainSignature, got {errors:?}"
    );
}

#[test]
fn main_returning_a_value_is_rejected() {
    let package = TestPackage::new("main() => i32 { return 0; }");
    let errors = compile_errors(&package, "a value-returning `main` must be rejected");
    assert!(
        has_invalid_main_signature(&errors),
        "expected InvalidMainSignature, got {errors:?}"
    );
}

#[test]
fn non_root_module_main_is_unaffected() {
    let package = TestPackage::new(
        r#"
        import self::helper;
        main() => void { helper::main(); }
        "#,
    );
    package.write_child("helper", "exposed main() => i32 { 0 }");
    package
        .compile()
        .expect("a non-root-module `main` keeps an ordinary signature");
}
