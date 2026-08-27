use omega_analyzer::Target;
use omega_analyzer::error::{AnalysisErrorKind, TypeResolutionError};
use omega_analyzer::resolver::ResolveError;
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

    fn compile_errors(&self, expectation: &str) -> Vec<CompileError> {
        match Driver::new(self.0.clone(), None, vec![], Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".into())], Target::DEFAULT)
        {
            Ok(_) => panic!("{expectation}"),
            Err(errors) => errors,
        }
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
        import self::helper::apply;
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
fn an_exposed_macro_may_name_a_hidden_item_of_its_own_module() {
    let package = TestPackage::new(
        r#"
        import self::helper::apply;
        entry_fn() => i32 { apply$(1) }
        "#,
    );
    package.child(
        "helper",
        r#"
        # Hidden, and the caller cannot name it; the macro body is authored
        # here, so ordinary definition-site visibility is all it needs.
        secret(value: i32) => i32 { value }
        exposed macro apply($value: expr) => { secret($value) }
        "#,
    );
    package.compile();
}

#[test]
fn a_definition_authored_reveal_reaches_past_the_definition_site() {
    let package = TestPackage::new(
        r#"
        import self::helper::read_tag;
        import self::model::Box;
        entry_fn() => i32 { b := Box::make(); read_tag$(b) }
        "#,
    );
    package.child(
        "model",
        r#"
        exposed struct Box {
            tag: i32;

            exposed make() => Box { Box { tag = 7; } }
        }
        "#,
    );
    package.child(
        "helper",
        r#"
        # `Box::tag` is hidden to `helper` as much as to `main`, so the macro
        # definition itself has to ask for the bypass.
        exposed macro read_tag($value: expr) => { reveal $value.tag }
        "#,
    );
    package.compile();
}

#[test]
fn a_caller_side_reveal_cannot_reach_a_macro_s_inaccessible_dependency() {
    let package = TestPackage::new(
        r#"
        import self::helper::read_tag;
        import self::model::Box;
        entry_fn() => i32 { b := Box::make(); reveal read_tag$(b) }
        "#,
    );
    package.child(
        "model",
        r#"
        exposed struct Box {
            tag: i32;

            exposed make() => Box { Box { tag = 7; } }
        }
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro read_tag($value: expr) => { $value.tag }
        "#,
    );
    let errors = package.compile_errors("a caller's reveal may not authorize the macro's own body");
    assert!(errors.iter().any(|error| matches!(
        error,
        CompileError::Analysis { errors, .. }
            if errors.iter().any(|error| matches!(
                error.kind,
                AnalysisErrorKind::FieldNotVisible { .. }
            ))
    )));
}

#[test]
fn a_definition_authored_reveal_reaches_past_a_multi_root_type_spelling() {
    let package = TestPackage::new(
        r#"
        import self::helper::widen;
        entry_fn() => i32 { n := 7; e := widen$(n); 0 }
        "#,
    );
    package.child("model", "struct Secret { exposed v: i32; }");
    package.child(
        "helper",
        r#"
        # `enum A | B` has no single root name, so the reveal has to be matched
        # against the origin of the member that actually needs it.
        exposed macro widen($x: expr) => { reveal <enum root::model::Secret | i32>$x }
        "#,
    );
    package.compile();
}

#[test]
fn a_caller_side_reveal_cannot_authorize_a_macro_authored_type_spelling() {
    let package = TestPackage::new(
        r#"
        import self::helper::widen;
        entry_fn() => i32 { n := 7; e := reveal widen$(n); 0 }
        "#,
    );
    package.child("model", "struct Secret { exposed v: i32; }");
    package.child(
        "helper",
        r#"
        exposed macro widen($x: expr) => { <enum root::model::Secret | i32>$x }
        "#,
    );
    let errors =
        package.compile_errors("a caller's reveal may not authorize a macro-authored type");
    assert!(errors.iter().any(|error| matches!(
        error,
        CompileError::Analysis { errors, .. }
            if errors.iter().any(|error| matches!(
                error.kind,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::ModuleResolution(
                    ResolveError::NotVisible { .. }
                ))
            ))
    )));
}

#[test]
fn a_caller_side_reveal_still_authorizes_its_own_substituted_type() {
    let package = TestPackage::new(
        r#"
        import self::helper::widen;
        entry_fn() => i32 { n := 7; e := reveal widen$(root::model::Secret, n); 0 }
        "#,
    );
    package.child("model", "struct Secret { exposed v: i32; }");
    package.child(
        "helper",
        r#"
        # `$T` carries the caller's origin even though the `enum` spelling
        # around it is macro-authored, so the caller's reveal reaches it.
        exposed macro widen($T: type, $x: expr) => { <enum $T | i32>$x }
        "#,
    );
    package.compile();
}

#[test]
fn a_macro_authored_member_name_does_not_inherit_the_invocation_site_owner() {
    let package = TestPackage::new(
        r#"
        import self::helper::read_tag;
        struct Box {
            tag: i32;

            exposed make() => Box { Box { tag = 7; } }
            # `read_tag$` expands inside `Box`'s own method, but the `.tag`
            # token it writes was authored in `helper`, which owns nothing.
            exposed leak(*self) => i32 { read_tag$(self) }
        }
        entry_fn() => i32 { b := Box::make(); b.leak() }
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro read_tag($value: expr) => { $value.tag }
        "#,
    );
    let errors = package.compile_errors("a macro-authored member name has no owner-only privilege");
    assert!(errors.iter().any(|error| matches!(
        error,
        CompileError::Analysis { errors, .. }
            if errors.iter().any(|error| matches!(
                error.kind,
                AnalysisErrorKind::FieldNotVisible { .. }
            ))
    )));
}

#[test]
fn a_macro_authored_member_name_may_reveal_a_hidden_member_itself() {
    let package = TestPackage::new(
        r#"
        import self::helper::read_tag;
        struct Box {
            tag: i32;

            exposed make() => Box { Box { tag = 7; } }
            exposed leak(*self) => i32 { read_tag$(self) }
        }
        entry_fn() => i32 { b := Box::make(); b.leak() }
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro read_tag($value: expr) => { reveal $value.tag }
        "#,
    );
    package.compile();
}

#[test]
fn a_macro_authored_member_name_reaches_a_shared_member_of_its_own_package() {
    let package = TestPackage::new(
        r#"
        import self::helper::read_tag;
        import self::model::Box;
        entry_fn() => i32 { b := Box::make(); read_tag$(b) }
        "#,
    );
    package.child(
        "model",
        r#"
        exposed struct Box {
            shared tag: i32;

            exposed make() => Box { Box { tag = 7; } }
        }
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro read_tag($value: expr) => { $value.tag }
        "#,
    );
    package.compile();
}

#[test]
fn macro_locals_do_not_capture_substituted_arguments() {
    let package = TestPackage::new(
        r#"
        import self::helper::keep;
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
        import self::helper::outer;
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
        import self::helper::takes_expr;
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

#[test]
fn a_macro_repetition_may_redeclare_its_own_local_each_time() {
    let package = TestPackage::new(
        r#"
        import self::helper::sum_each;
        entry_fn() => i32 { sum_each$(1, 2, 3) }
        "#,
    );
    package.child(
        "helper",
        r#"
        # Every repetition expands into the same caller block under the same
        # macro origin, so `item` shadows the previous repetition's binding
        # rather than colliding with it.
        exposed macro sum_each($value: expr...) => {
            {
                mut total := 0;
                $...(){
                    item := $value;
                    total += item;
                }
                total
            }
        }
        "#,
    );
    package.compile();
}
