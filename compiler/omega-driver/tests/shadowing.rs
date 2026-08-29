use omega_analyzer::Target;
use omega_analyzer::error::{AnalysisWarning, AnalysisWarningKind};
use omega_driver::{Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// Shadowing keeps every declaration alive for diagnostics: the shadowed
/// binding is unreachable by name afterwards, but it still answers for its own
/// unused/`mut` warnings. These are component tests because the conformance
/// runner only compares a successfully compiled program's *runtime* output,
/// so compiler warnings are not observable there.
struct TestPackage(PathBuf);

impl TestPackage {
    fn new(body: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_shadowing_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(
            root.join("main.omg"),
            format!("entry_fn() => i32 {{ {body} }}"),
        )
        .expect("write root module");
        Self(root)
    }

    fn warnings(&self) -> Vec<AnalysisWarning> {
        Driver::new(
            self.0.clone(),
            None,
            Vec::<ExternRoot>::new(),
            Target::DEFAULT,
        )
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
        .expect("package should compile")
        .warnings
        .into_iter()
        .map(|(_, warning)| warning)
        .collect()
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn unused_variables(warnings: &[AnalysisWarning]) -> Vec<String> {
    warnings
        .iter()
        .filter_map(|warning| match &warning.kind {
            AnalysisWarningKind::UnusedVariable { name } => Some(name.0.clone()),
            _ => None,
        })
        .collect()
}

fn unnecessary_muts(warnings: &[AnalysisWarning]) -> Vec<String> {
    warnings
        .iter()
        .filter_map(|warning| match &warning.kind {
            AnalysisWarningKind::UnnecessaryMut { name } => Some(name.0.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_shadowed_binding_still_reports_its_own_unused_warning() {
    let warnings = TestPackage::new("x := 1; x := 2; x").warnings();
    assert_eq!(
        unused_variables(&warnings),
        vec!["x".to_string()],
        "the shadowed declaration is never read and must still warn exactly once"
    );
}

#[test]
fn a_shadowing_initializer_uses_the_binding_it_shadows() {
    let warnings = TestPackage::new("x := 1; x := x + 1; x").warnings();
    assert!(
        unused_variables(&warnings).is_empty(),
        "the initializer reads the previous binding, so neither is unused: {warnings:#?}"
    );
}

#[test]
fn a_shadowed_binding_still_reports_its_own_unnecessary_mut() {
    let warnings = TestPackage::new("mut x := 1; x := x + 1; x").warnings();
    assert_eq!(
        unnecessary_muts(&warnings),
        vec!["x".to_string()],
        "dropping the privilege by shadowing does not excuse the unwritten `mut`"
    );
}

#[test]
fn an_inner_scope_shadow_does_not_silence_the_outer_binding() {
    let warnings = TestPackage::new("x := 1; y := { x := 2; x }; x + y").warnings();
    assert!(
        unused_variables(&warnings).is_empty(),
        "both declarations are read: {warnings:#?}"
    );
}
