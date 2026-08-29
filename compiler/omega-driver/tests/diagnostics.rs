//! Frontend diagnostic recovery and attribution: one failure must not hide
//! unrelated ones, and every finding must point at the source a developer can
//! act on.

use omega_analyzer::Target;
use omega_analyzer::error::{
    AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind,
};
use omega_driver::{CompileError, CompiledProgram, Driver};
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
            "omega_diagnostics_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn child(&self, name: &str, source: &str) -> &Self {
        fs::write(self.0.join(format!("{name}.omg")), source).expect("write child module");
        self
    }

    fn result(&self) -> Result<CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, vec![], Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".into())], Target::DEFAULT)
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.result() {
            Ok(_) => panic!("expected this package to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }

    fn expect_ok(&self) -> CompiledProgram {
        match self.result() {
            Ok(program) => program,
            Err(errors) => panic!("expected this package to compile, got: {errors:#?}"),
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn modules_with_errors(errors: &[CompileError]) -> Vec<String> {
    errors
        .iter()
        .filter_map(|error| error.module())
        .map(|module| {
            module
                .iter()
                .map(Ident::as_ref)
                .collect::<Vec<_>>()
                .join("::")
        })
        .collect()
}

fn analysis_errors(errors: &[CompileError]) -> Vec<&AnalysisError> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors.iter().collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn rendered(errors: &[CompileError]) -> String {
    errors
        .iter()
        .flat_map(CompileError::to_diagnostics)
        .map(|d| d.message)
        .collect::<Vec<_>>()
        .join("\n")
}

fn warnings(program: &CompiledProgram) -> Vec<&AnalysisWarning> {
    program
        .warnings
        .iter()
        .map(|(_, warning)| warning)
        .collect()
}

#[test]
fn a_broken_module_does_not_hide_a_sibling_modules_errors() {
    let package = TestPackage::new(
        r#"
        import self::broken;
        import self::healthy;

        main() => void { }
        "#,
    );
    package.child("broken", "this is not omega syntax {{{");
    package.child(
        "healthy",
        r#"
        exposed use_undefined() => i32 { no_such_name }
        "#,
    );

    let errors = package.expect_errors();
    let modules = modules_with_errors(&errors);
    assert!(
        modules.iter().any(|m| m == "main::broken"),
        "the parse failure must be reported: {modules:?}"
    );
    assert!(
        modules.iter().any(|m| m == "main::healthy"),
        "an unrelated module's own error must survive the broken one: {modules:?}"
    );
}

#[test]
fn independent_errors_in_one_module_are_reported_together() {
    let package = TestPackage::new(
        r#"
        first() => i32 { missing_one }

        second() => i32 { missing_two }

        main() => void { }
        "#,
    );

    let errors = package.expect_errors();
    let text = rendered(&errors);
    assert!(
        text.contains("missing_one") && text.contains("missing_two"),
        "each independent failure must be reported: {text}"
    );
}

#[test]
fn a_failed_signature_skips_only_its_own_body() {
    let package = TestPackage::new(
        r#"
        broken(value: NoSuchType) => i32 { value }

        healthy() => i32 { also_missing }

        main() => void { }
        "#,
    );

    let errors = package.expect_errors();
    let text = rendered(&errors);
    assert!(
        text.contains("NoSuchType"),
        "the failing signature must be reported: {text}"
    );
    assert!(
        text.contains("also_missing"),
        "a healthy signature's body must still be analyzed: {text}"
    );
}

#[test]
fn a_dependent_use_never_replaces_the_primary_with_a_rootless_marker() {
    // A generic and a non-generic candidate share one name. Whatever the
    // overload machinery does with that, the real reason must be visible.
    let package = TestPackage::new(
        r#"
        free<T>(p: *T) => void { }

        free(p: *u8) => void { }

        main() => void { }
        "#,
    );

    let errors = package.expect_errors();
    let text = rendered(&errors);
    assert!(
        !text.is_empty(),
        "the failure must produce at least one diagnostic"
    );
    assert!(
        !text
            .lines()
            .all(|line| line.contains("because of its own error")),
        "the primary reason must be present, not only the secondary marker: {text}"
    );
}

#[test]
fn a_macro_authored_unused_local_is_diagnosed_at_the_macro_definition() {
    let package = TestPackage::new(
        r#"
        import self::defs::declare_unused;

        main() => void {
            declare_unused$();
        }
        "#,
    );
    package.child(
        "defs",
        r#"
        exposed macro declare_unused() => {
            spare := 1;
        }
        "#,
    );

    let program = package.expect_ok();
    let unused: Vec<_> = warnings(&program)
        .into_iter()
        .filter(|warning| matches!(warning.kind, AnalysisWarningKind::UnusedVariable { .. }))
        .collect();
    assert_eq!(
        unused.len(),
        1,
        "the macro-authored binding is diagnosed once: {unused:?}"
    );
    let authored = unused[0]
        .authored
        .as_ref()
        .expect("macro-authored syntax must record where it was written");
    assert_eq!(authored.macro_name.as_ref(), "declare_unused");
}

#[test]
fn a_caller_substituted_binding_stays_the_callers_own_finding() {
    let package = TestPackage::new(
        r#"
        import self::defs::bind;

        main() => void {
            bind$(caller_spare);
        }
        "#,
    );
    package.child(
        "defs",
        r#"
        exposed macro bind($name: ident) => {
            $name := 1;
        }
        "#,
    );

    let program = package.expect_ok();
    let unused: Vec<_> = warnings(&program)
        .into_iter()
        .filter(|warning| matches!(warning.kind, AnalysisWarningKind::UnusedVariable { .. }))
        .collect();
    assert_eq!(unused.len(), 1, "{unused:?}");
    assert!(
        unused[0].authored.is_none(),
        "the caller wrote this name, so the caller owns the finding: {:?}",
        unused[0]
    );
}

#[test]
fn a_macro_invoked_twice_reports_its_authored_finding_once() {
    let package = TestPackage::new(
        r#"
        import self::defs::declare_unused;

        main() => void {
            declare_unused$();
            declare_unused$();
        }
        "#,
    );
    package.child(
        "defs",
        r#"
        exposed macro declare_unused() => {
            spare := 1;
        }
        "#,
    );

    let program = package.expect_ok();
    let unused: Vec<_> = warnings(&program)
        .into_iter()
        .filter(|warning| matches!(warning.kind, AnalysisWarningKind::UnusedVariable { .. }))
        .collect();
    assert_eq!(
        unused.len(),
        1,
        "one macro definition is one actionable source site: {unused:?}"
    );
}

#[test]
fn a_generic_declarations_warning_is_not_repeated_per_instantiation() {
    let package = TestPackage::new(
        r#"
        holds<T>(value: T) => T {
            spare := 1;
            value
        }

        main() => void {
            <void>holds(1u8);
            <void>holds(2u32);
        }
        "#,
    );

    let program = package.expect_ok();
    let unused: Vec<_> = warnings(&program)
        .into_iter()
        .filter(|warning| matches!(warning.kind, AnalysisWarningKind::UnusedVariable { .. }))
        .collect();
    assert_eq!(
        unused.len(),
        1,
        "one written binding is one finding, however many instantiations exist: {unused:?}"
    );
}

#[test]
fn a_cross_module_duplicate_conformance_labels_both_declarations() {
    let package = TestPackage::new(
        r#"
        import self::second;

        exposed spec Countable {
            count(*self) => i32;
        }

        exposed struct Widget {
            exposed n: i32;
        }

        meet Countable for Widget {
            count(*self) => i32 { self.n }
        }

        main() => void { }
        "#,
    );
    package.child(
        "second",
        r#"
        import root::Countable;
        import root::Widget;

        meet Countable for Widget {
            count(*self) => i32 { 0 }
        }
        "#,
    );

    let errors = package.expect_errors();
    let duplicate = analysis_errors(&errors)
        .into_iter()
        .find(|error| {
            matches!(
                error.kind,
                AnalysisErrorKind::DuplicateConformance { .. }
                    | AnalysisErrorKind::AmbiguousConformance { .. }
            )
        })
        .unwrap_or_else(|| panic!("expected a duplicate conformance: {}", rendered(&errors)));

    let other = match &duplicate.kind {
        AnalysisErrorKind::DuplicateConformance { previous, .. } => *previous,
        AnalysisErrorKind::AmbiguousConformance { first, .. } => *first,
        other => panic!("unexpected kind {other:?}"),
    };
    assert!(
        other.is_some(),
        "the other conformance must name its own source, not this module's byte space"
    );
}

#[test]
fn conflicting_glues_label_each_glue_block_in_its_own_source() {
    let package = TestPackage::new(
        r#"
        import self::a;
        import self::b;

        gap logger {
            log() => void;
        }

        main() => void { logger::log(); }
        "#,
    );
    package.child(
        "a",
        r#"
        import root::logger;

        glue logger {
            log() => void { }
        }
        "#,
    );
    package.child(
        "b",
        r#"
        import root::logger;

        glue logger {
            log() => void { }
        }
        "#,
    );

    let errors = package.expect_errors();
    let conflict = analysis_errors(&errors)
        .into_iter()
        .find(|error| matches!(error.kind, AnalysisErrorKind::MultipleGluesForGap { .. }))
        .unwrap_or_else(|| panic!("expected a glue conflict: {}", rendered(&errors)));

    let AnalysisErrorKind::MultipleGluesForGap { glues, .. } = &conflict.kind else {
        unreachable!("matched above")
    };
    assert_eq!(glues.len(), 2, "{glues:?}");
    assert_ne!(
        glues[0].source, glues[1].source,
        "each conflicting glue must name the file it was written in: {glues:?}"
    );
}
