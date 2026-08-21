use omega_analyzer::Target;
use omega_analyzer::error::{AnalysisErrorKind, AnalysisWarningKind};
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
            "omega_pointer_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn result(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(
            self.0.clone(),
            None,
            vec![ExternRoot {
                name: Ident("core".to_string()),
                dir: core_root(),
            }],
            Target::DEFAULT,
        )
        .expect("construct driver with the real core extern")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    fn expect_ok(&self) {
        if let Err(errors) = self.result() {
            panic!("expected this to compile, got: {errors:#?}");
        }
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.result() {
            Ok(_) => panic!("expected this to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn core_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/core")
        .canonicalize()
        .expect("runtime/core exists")
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

fn with_two_pointers(body: &str) -> String {
    format!(
        "entry_fn() => i32 {{ values : [2]u8 = [1u8, 2u8]; a := &values[0]; b := &values[1]; {body} }}"
    )
}

#[test]
fn arithmetic_between_two_pointers_is_rejected() {
    for expression in ["a + b", "a * b", "a / b", "a & b", "a | b", "a ^ b"] {
        let source = with_two_pointers(&format!("x := {expression}; 0"));
        let package = TestPackage::new(&source);
        assert!(
            has_analysis_error(&package.expect_errors(), |kind| matches!(
                kind,
                AnalysisErrorKind::PointerPairArithmetic { .. }
            )),
            "expected PointerPairArithmetic for: {expression}"
        );
    }
}

#[test]
fn subtracting_two_pointers_is_still_allowed() {
    TestPackage::new(&with_two_pointers("x := b - a; <i32>x")).expect_ok();
}

#[test]
fn comparing_two_pointers_is_still_allowed() {
    TestPackage::new(&with_two_pointers(
        "if a == b { return 1; } if a < b { 0 } else { 2 }",
    ))
    .expect_ok();
}

#[test]
fn offsetting_a_pointer_by_an_integer_is_still_allowed() {
    TestPackage::new(&with_two_pointers(
        "forward := a + 1usize; back := forward - 1usize; if back == a { 0 } else { 1 }",
    ))
    .expect_ok();
}

#[test]
fn taking_a_binding_s_address_counts_as_using_it() {
    let program = TestPackage::new("entry_fn() => i32 { a := 5; p := &a; *p }")
        .result()
        .expect("must compile");
    assert!(
        !program
            .warnings
            .iter()
            .any(|(_, warning)| matches!(warning.kind, AnalysisWarningKind::UnusedVariable { .. })),
        "taking `&a` must not leave `a` reported as unused"
    );
}
