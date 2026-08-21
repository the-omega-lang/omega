use omega_analyzer::Target;
use omega_analyzer::error::AnalysisErrorKind;
use omega_driver::{CompileError, Driver};
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
            "omega_macro_hygiene_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn child(&self, name: &str, source: &str) {
        fs::write(self.0.join(format!("{name}.omg")), source).expect("write child module");
    }

    fn compile(&self) {
        Driver::new(self.0.clone(), None, vec![], Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".into())], Target::DEFAULT)
            .expect("package should compile");
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

#[test]
fn macro_body_items_resolve_in_its_definition_module() {
    let package = TestPackage::new(
        r#"
        import helper::apply;
        # This caller-local declaration deliberately shadows the helper's
        # name. The macro body must still select helper::add_one.
        add_one(value: i32) => i32 { value + 100 }
        entry_fn() => i32 { apply$(1) }
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro apply($value: expr) => { add_one($value) }
        exposed add_one(value: i32) => i32 { value + 1 }
        "#,
    );
    package.compile();
}

#[test]
fn an_exposed_macro_cannot_name_a_hidden_item() {
    let package = TestPackage::new(
        r#"
        import helper::apply;
        entry_fn() => i32 { apply$(1) }
        "#,
    );
    package.child(
        "helper",
        r#"
        secret(value: i32) => i32 { value }
        exposed macro apply($value: expr) => { secret($value) }
        "#,
    );
    let result = Driver::new(package.0.clone(), None, vec![], Target::DEFAULT)
        .expect("construct driver")
        .compile(&[Ident("main".into())], Target::DEFAULT);
    let errors = match result {
        Ok(_) => panic!("an exposed macro may not expose a hidden dependency"),
        Err(errors) => errors,
    };
    assert!(errors.iter().any(|error| matches!(
        error,
        CompileError::Analysis { errors, .. }
            if errors.iter().any(|error| matches!(error.kind, AnalysisErrorKind::MacroDependencyTooPrivate { .. }))
    )));
}

#[test]
fn macro_locals_do_not_capture_substituted_arguments() {
    let package = TestPackage::new(
        r#"
        import helper::keep;
        entry_fn() => i32 { out := 7; keep$(out); out }
        "#,
    );
    package.child(
        "helper",
        r#"
        # If `$value` were captured by this `out`, `out + 1` would try to
        # add an integer to a bool and fail type checking.
        exposed macro keep($value: expr) => { out := true; $value + 1; }
        "#,
    );
    package.compile();
}

#[test]
fn nested_macro_calls_use_the_definition_environment() {
    let package = TestPackage::new(
        r#"
        import helper::outer;
        entry_fn() => i32 { outer$(41) }
        "#,
    );
    package.child(
        "helper",
        r#"
        macro inner($value: expr) => { $value + 1 }
        exposed macro outer($value: expr) => { inner$($value) }
        "#,
    );
    package.compile();
}

#[test]
fn a_macro_invocation_passed_as_an_argument_resolves_at_the_call_site() {
    let package = TestPackage::new(
        r#"
        import helper::takes_expr;
        macro caller_macro($a: expr) => { ($a) * 2 }
        entry_fn() => i32 { takes_expr$(caller_macro$(20)) }
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro takes_expr($value: expr) => { ($value) + 1 }
        "#,
    );
    package.compile();
}
