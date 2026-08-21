use omega_analyzer::Target;
use omega_analyzer::error::AnalysisWarningKind;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::diagnostics::ParseErrorKind;
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
            "omega_hidden_visibility_test_{}_{}",
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

    fn expect_ok(&self) -> omega_driver::CompiledProgram {
        match self.result() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
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

fn has_parse_error(errors: &[CompileError], predicate: impl Fn(&ParseErrorKind) -> bool) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Parse { errors, .. } => errors.iter().any(|error| predicate(&error.kind)),
        _ => false,
    })
}

#[test]
fn a_default_spec_body_can_call_a_hidden_sibling_member() {
    let program = TestPackage::new(
        r#"
        shared spec Greeter {
            name(*self) => i32;

            hidden double_name(*self) => i32 {
                self.name() * 2
            }

            greet(*self) => i32 {
                self.double_name() + 1
            }
        }

        struct Dog {
            exposed id: i32;
        }

        conform Dog to Greeter {
            name(*self) => i32 { self.id }
        }

        entry_fn() => i32 {
            dog := Dog { id = 5; };
            Greeter::greet(&dog)
        }
        "#,
    )
    .expect_ok();
    assert!(
        !program
            .warnings
            .iter()
            .any(|(_, warning)| matches!(
                warning.kind,
                AnalysisWarningKind::RedundantHiddenModifier
            )),
        "narrowing a shared spec member to hidden is not redundant"
    );
}

#[test]
fn a_spec_member_visibility_exceeding_its_spec_is_rejected() {
    let package = TestPackage::new(
        r#"
        shared spec Loud {
            exposed shout(*self) => i32;
        }

        entry_fn() => i32 { 0 }
        "#,
    );
    assert!(
        has_parse_error(&package.expect_errors(), |kind| matches!(
            kind,
            ParseErrorKind::SpecMethodVisibilityExceedsSpec { .. }
        )),
        "expected SpecMethodVisibilityExceedsSpec"
    );
}

#[test]
fn an_explicit_hidden_on_an_ordinary_field_is_redundant() {
    let program = TestPackage::new(
        r#"
        struct Box {
            hidden data: i32;

            exposed new(v: i32) => Self {
                Self { data = v; }
            }
        }

        entry_fn() => i32 {
            b := Box::new(1);
            reveal b.data
        }
        "#,
    )
    .expect_ok();
    assert!(
        program
            .warnings
            .iter()
            .any(|(_, warning)| matches!(
                warning.kind,
                AnalysisWarningKind::RedundantHiddenModifier
            )),
        "explicit 'hidden' on an already-hidden field should warn"
    );
}
