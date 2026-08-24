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
            "omega_cast_test_{}_{}",
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

fn has_analysis_error(
    errors: &[CompileError],
    predicate: impl Fn(&AnalysisErrorKind) -> bool,
) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| predicate(&error.kind)),
        _ => false,
    })
}

fn warnings(program: &omega_driver::CompiledProgram) -> Vec<&AnalysisWarningKind> {
    program
        .warnings
        .iter()
        .map(|(_, warning)| &warning.kind)
        .collect()
}

const PRELUDE: &str = "\
produce() => i32 { 7 }
consume() => void { }
add(a: i32, b: i32) => i32 { a + b }
";

fn in_main(body: &str) -> String {
    format!("{PRELUDE}main() => void {{ {body} }}")
}

#[test]
fn void_cast_discards_a_call_result_without_warning() {
    let program = TestPackage::new(&in_main("<void>produce();")).expect_ok();
    let kinds = warnings(&program);
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::UnusedReturnValue)),
        "`<void>` is the explicit acknowledgement, so nothing is unused: {kinds:#?}"
    );
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::UnusedCastResult { .. })),
        "a `void` result is not a discarded cast result: {kinds:#?}"
    );
}

#[test]
fn void_cast_of_a_void_operand_is_not_a_no_op_cast() {
    let program = TestPackage::new(&in_main("<void>consume();")).expect_ok();
    let kinds = warnings(&program);
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::NoOpCast { .. })),
        "a `<void>` discard is intentional by definition: {kinds:#?}"
    );
}

#[test]
fn void_cast_keeps_a_diverging_operand_divergent() {
    // The `i32` body has no tail value, so this only compiles if the discard
    // is still recognized as diverging.
    TestPackage::new(&format!(
        "{PRELUDE}spin() => never {{ loop {{ }} }}\nentry() => i32 {{ <void>spin(); }}\nmain() => void {{ }}"
    ))
    .expect_ok();
}

#[test]
fn a_bare_cast_statement_warns_that_its_result_is_discarded() {
    let program = TestPackage::new(&in_main("<i64>produce();")).expect_ok();
    let kinds = warnings(&program);
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::UnusedCastResult { .. })),
        "expected `unused_cast_result`, got: {kinds:#?}"
    );
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::UnusedReturnValue)),
        "the cast statement rule replaces the call rule here: {kinds:#?}"
    );
}

#[test]
fn a_consumed_cast_does_not_warn() {
    let program =
        TestPackage::new(&in_main("widened := <i64>produce(); <void>widened;")).expect_ok();
    let kinds = warnings(&program);
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::UnusedCastResult { .. })),
        "a cast bound to a local is used: {kinds:#?}"
    );
}

#[test]
fn a_bare_anonymous_enum_cast_statement_still_warns() {
    // Analysis lowers this to a conversion node rather than `CheckedExpr::Cast`,
    // so the statement rule has to look at the source form.
    let program = TestPackage::new(&format!(
        "{PRELUDE}main() => void {{ <enum i32 | bool>produce(); }}"
    ))
    .expect_ok();
    let kinds = warnings(&program);
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::UnusedCastResult { .. })),
        "expected `unused_cast_result`, got: {kinds:#?}"
    );
}

#[test]
fn a_function_round_trips_through_a_thin_raw_pointer() {
    TestPackage::new(&in_main(
        "address := <*void>add; back := <(a: i32, b: i32) => i32>address; <void>back(1, 2);",
    ))
    .expect_ok();
}

#[test]
fn a_function_casts_to_its_own_type_as_a_no_op() {
    let program = TestPackage::new(&in_main(
        "same := <(a: i32, b: i32) => i32>add; <void>same;",
    ))
    .expect_ok();
    let kinds = warnings(&program);
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, AnalysisWarningKind::NoOpCast { .. })),
        "an identity function cast is exactly the no-op case: {kinds:#?}"
    );
}

#[test]
fn a_direct_cast_between_differing_function_types_is_rejected() {
    for target in [
        "foreign(c) (a: i32, b: i32) => i32",
        "(a: i32, b: i32) => i64",
        "(a: i32) => i32",
    ] {
        let package = TestPackage::new(&in_main(&format!("bad := <{target}>add;")));
        assert!(
            has_analysis_error(&package.expect_errors(), |kind| matches!(
                kind,
                AnalysisErrorKind::InvalidCast { .. }
            )),
            "expected InvalidCast when casting `add` to `{target}`"
        );
    }
}

#[test]
fn a_function_does_not_cast_to_or_from_an_integer() {
    for body in [
        "bits := <usize>add;",
        "back := <(a: i32, b: i32) => i32>1usize;",
    ] {
        let package = TestPackage::new(&in_main(body));
        assert!(
            has_analysis_error(&package.expect_errors(), |kind| matches!(
                kind,
                AnalysisErrorKind::InvalidCast { .. }
            )),
            "expected InvalidCast for: {body}"
        );
    }
}

#[test]
fn a_function_does_not_cast_to_a_mutable_pointer() {
    let package = TestPackage::new(&in_main("writable := <*mut void>add;"));
    assert!(
        has_analysis_error(&package.expect_errors(), |kind| matches!(
            kind,
            AnalysisErrorKind::CastToMutablePointer { .. }
        )),
        "a function value must not manufacture writable data access"
    );
}

#[test]
fn a_fat_pointer_is_not_a_bridge_to_a_function() {
    let package = TestPackage::new(&in_main(
        "text: *str = \"x\"; bad := <(a: i32, b: i32) => i32>text;",
    ));
    assert!(
        has_analysis_error(&package.expect_errors(), |kind| matches!(
            kind,
            AnalysisErrorKind::InvalidCast { .. }
        )),
        "only a thin raw pointer is one pointer leaf"
    );
}

#[test]
fn casting_to_a_foreign_function_type_does_not_validate_its_aggregate_abi() {
    // Constructing/holding the pointer is not the ABI boundary; only a call
    // through it is. See docs/language/foreign-function-interface.md.
    TestPackage::new(&format!(
        "{PRELUDE}struct Pair {{ exposed a: i32; exposed b: i32; }}\n\
         main() => void {{ address := <*void>add; fp := <foreign(c) (value: Pair) => void>address; <void>fp; }}"
    ))
    .expect_ok();
}
