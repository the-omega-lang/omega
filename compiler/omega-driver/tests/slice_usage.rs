use omega_analyzer::Target;
use omega_analyzer::error::{AnalysisWarning, AnalysisWarningKind};
use omega_driver::{Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

fn warnings(body: &str) -> Vec<AnalysisWarning> {
    let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let parent: PathBuf = std::env::temp_dir().join(format!(
        "omega_slice_usage_test_{}_{}",
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
    let result = Driver::new(root, None, Vec::<ExternRoot>::new(), Target::DEFAULT)
        .expect("construct driver")
        .compile(&omega_hir::ModulePath::new([Ident("main".to_string())]));
    let _ = fs::remove_dir_all(&parent);
    result
        .expect("package should compile")
        .warnings
        .into_iter()
        .map(|(_, warning)| warning)
        .collect()
}

fn binding_warnings(warnings: &[AnalysisWarning]) -> Vec<&AnalysisWarningKind> {
    warnings
        .iter()
        .map(|warning| &warning.kind)
        .filter(|kind| {
            matches!(
                kind,
                AnalysisWarningKind::UnusedVariable { .. }
                    | AnalysisWarningKind::UnnecessaryMut { .. }
            )
        })
        .collect()
}

#[test]
fn a_mutable_slice_of_an_otherwise_unread_array_uses_and_writes_it() {
    let warnings = warnings("mut a: [4]i32; s := &mut a[0..]; s[0] = 1; 0");
    assert!(
        binding_warnings(&warnings).is_empty(),
        "slicing reads `a` and `&mut` needs its `mut`: {warnings:#?}"
    );
}

#[test]
fn an_immutable_slice_of_an_otherwise_unread_array_uses_it() {
    let warnings = warnings("a: [4]i32 = [1, 2, 3, 4]; s := &a[1..]; s[0]");
    assert!(
        binding_warnings(&warnings).is_empty(),
        "slicing reads `a`: {warnings:#?}"
    );
}

#[test]
fn an_immutable_slice_does_not_excuse_an_unwritten_mut() {
    let warnings = warnings("mut a: [4]i32 = [1, 2, 3, 4]; s := &a[1..]; s[0]");
    assert!(
        matches!(
            binding_warnings(&warnings).as_slice(),
            [AnalysisWarningKind::UnnecessaryMut { name }] if name.0 == "a"
        ),
        "{warnings:#?}"
    );
}
