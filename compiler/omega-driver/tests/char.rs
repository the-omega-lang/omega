
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
            "omega_char_test_{}_{}",
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
            }], Target::DEFAULT)
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

#[test]
fn u8_casts_into_char_and_char_casts_out_to_any_integer() {
    TestPackage::new(
        r#"
        main() => i32 {
            c := <char>65u8;
            if c == 'A' { <i32>c } else { <i32><u32>c }
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_arbitrary_integer_does_not_cast_into_char() {
    let package = TestPackage::new("main() => i32 { c := <char>65; <i32>c }");
    assert!(!package.expect_errors().is_empty());
}

#[test]
fn from_u32_is_available_and_returns_an_option() {
    TestPackage::new(
        r#"
        import extern::core::option::Option;
        main() => i32 {
            checked := char::from_u32(0x41u32);
            match checked {
                Option::Some => { <i32>checked.value },
            } else { 1 }
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn every_char_classifier_is_callable() {
    TestPackage::new(
        r#"
        main() => i32 {
            c := 'a';
            if c.is_ascii() { } else { return 1; }
            if c.is_digit() { return 2; }
            if c.is_alphabetic() { } else { return 3; }
            if c.is_whitespace() { return 4; }
            upper := c.to_ascii_uppercase();
            lower := upper.to_ascii_lowercase();
            if lower != c { return 5; }
            <i32>c.len_utf8()
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn char_satisfies_an_ord_bound() {
    TestPackage::new(
        r#"
        import extern::core::cmp::Ord;
        biggest<T: Ord>(a: T, b: T) => bool { a.greater_than(b) }
        main() => i32 { if biggest('b', 'a') { 0 } else { 1 } }
        "#,
    )
    .expect_ok();
}

#[test]
fn char_conforms_to_bounded_and_successor() {
    TestPackage::new(
        r#"
        import extern::core::range::Successor;
        import extern::core::option::Option;
        step<T: Successor>(value: T) => Option<T> { value.successor() }
        main() => i32 {
            first : char = char::min();
            next := step('a');
            match next {
                Option::Some => { if next.value == 'b' { <i32>first } else { 1 } },
            } else { 2 }
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn every_arithmetic_spelling_on_char_is_rejected() {
    for source in [
        "main() => i32 { a := 'a'; b := 'b'; x := a + b; 0 }",
        "main() => i32 { a := 'a'; x := a + 1; 0 }",
        "main() => i32 { a := 'a'; b := 'b'; x := a - b; 0 }",
        "main() => i32 { a := 'a'; b := 'b'; x := a & b; 0 }",
        "main() => i32 { a := 'a'; x := ~a; 0 }",
        "main() => i32 { a := 'a'; x := -a; 0 }",
    ] {
        let package = TestPackage::new(source);
        assert!(
            has_analysis_error(&package.expect_errors(), |kind| matches!(
                kind,
                AnalysisErrorKind::CharArithmeticNotAllowed { .. }
            )),
            "expected CharArithmeticNotAllowed for: {source}"
        );
    }
}

#[test]
fn char_comparison_and_match_ranges_still_work() {
    TestPackage::new(
        r#"
        classify(c: char) => i32 {
            match c {
                'A'..='Z' => { 1 },
                'a'..='z' => { 2 },
                '0'..='9' => { 3 },
            } else { 0 }
        }
        main() => i32 {
            if 'a' < 'b' { } else { return 1; }
            if 'a' == 'a' { } else { return 2; }
            if 'b' >= 'a' { } else { return 3; }
            classify('Q')
        }
        "#,
    )
    .expect_ok();
}
