//! The compiler reaches `core` declarations by absolute path and checks their
//! shape where a program uses the feature built on them, so a `core` that
//! omits or reshapes one is a diagnostic naming what is wrong rather than a
//! silently dropped body or a compiler panic.

use omega_analyzer::Target;
use omega_analyzer::core_contract::CoreFeature;
use omega_analyzer::error::AnalysisErrorKind;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// A package compiled against its own copy of the repository's `core`.
struct Workspace(PathBuf);

impl Workspace {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "omega_core_contract_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(root.join("main")).expect("create test package");
        fs::write(root.join("main/main.omg"), source).expect("write root module");
        copy_tree(&real_core(), &root.join("core"));
        Self(root)
    }

    /// Replaces `from` with `to` in one file of this workspace's `core`.
    fn patch_core(&self, file: &str, from: &str, to: &str) -> &Self {
        let path = self.0.join("core").join(file);
        let source = fs::read_to_string(&path).expect("read core source");
        assert!(
            source.contains(from),
            "'{file}' no longer contains {from:?}"
        );
        fs::write(&path, source.replacen(from, to, 1)).expect("write core source");
        self
    }

    fn remove_core_file(&self, file: &str) -> &Self {
        fs::remove_file(self.0.join("core").join(file)).expect("remove core source");
        self
    }

    fn errors(&self) -> Vec<CompileError> {
        let result = Driver::new(
            self.0.join("main"),
            None,
            vec![ExternRoot {
                name: Ident("core".to_string()),
                dir: self.0.join("core"),
            }],
            Target::DEFAULT,
        )
        .expect("construct driver")
        .compile(&omega_hir::ModulePath::new([Ident("main".to_string())]))
        .result;
        match result {
            Ok(_) => panic!("expected this package to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }

    /// The one contract failure this package reports, as its detail.
    fn unavailable(&self, expected: CoreFeature) -> String {
        let errors = self.errors();
        let details: Vec<String> = errors
            .iter()
            .flat_map(|error| match error {
                CompileError::Analysis { errors, .. } => errors.iter().collect(),
                _ => Vec::new(),
            })
            .filter_map(|error| match &error.kind {
                AnalysisErrorKind::CoreSupportUnavailable { feature, detail }
                    if *feature == expected =>
                {
                    Some(detail.clone())
                }
                _ => None,
            })
            .collect();
        match details.as_slice() {
            [detail] => detail.clone(),
            _ => panic!("expected one {expected} contract failure, got: {errors:#?}"),
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn real_core() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/core")
        .canonicalize()
        .expect("runtime/core exists")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create core copy");
    for entry in fs::read_dir(from).expect("read core") {
        let entry = entry.expect("core entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("core entry type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy core source");
        }
    }
}

const RANGE_USE: &str = "helper() => i32 { r := 0..<3; r.start }\nmain() => void { helper(); }";
const LOOP_USE: &str = "helper() => void { for item in 0..<3 { } }\nmain() => void { helper(); }";

#[test]
fn a_range_without_core_range_is_reported_not_dropped() {
    let detail = Workspace::new(RANGE_USE)
        .remove_core_file("range.omg")
        .unavailable(CoreFeature::Ranges);
    assert_eq!(detail, "'core' declares no type 'core::range::Range'");
}

#[test]
fn a_reordered_range_is_rejected_rather_than_built_by_the_wrong_index() {
    let workspace = Workspace::new(RANGE_USE);
    workspace.patch_core(
        "range.omg",
        "    exposed start: T;\n    exposed end: T;",
        "    exposed end: T;\n    exposed start: T;",
    );
    assert!(
        workspace
            .unavailable(CoreFeature::Ranges)
            .contains("'start: T', 'end: T' and 'inclusive: bool', in that order")
    );
}

#[test]
fn an_inferred_bound_needs_bounded_min_and_max() {
    let workspace =
        Workspace::new("helper() => i32 { r := ..<3; r.start }\nmain() => void { helper(); }");
    workspace.patch_core("range.omg", "    min() => Self;\n", "");
    assert_eq!(
        workspace.unavailable(CoreFeature::Ranges),
        "'core::range::Bounded' must declare 'min() => Self'"
    );
}

#[test]
fn a_loop_without_core_iterator_is_reported() {
    let detail = Workspace::new(LOOP_USE)
        .patch_core(
            "iterator.omg",
            "exposed spec ToIterator<T>",
            "exposed spec Producer<T>",
        )
        .unavailable(CoreFeature::Iteration);
    assert_eq!(
        detail,
        "'core' declares no spec 'core::iterator::ToIterator'"
    );
}

#[test]
fn a_loop_needs_next_to_return_core_option() {
    let workspace = Workspace::new(LOOP_USE);
    workspace.patch_core(
        "option.omg",
        "    None,\n    Some { exposed value: T; };",
        "    Some { exposed value: T; },\n    None;",
    );
    assert!(
        workspace
            .unavailable(CoreFeature::Iteration)
            .contains("exactly 'None' and then 'Some { value }'")
    );
}

#[test]
fn the_try_operator_checks_the_shape_of_core_result() {
    let workspace = Workspace::new(
        "pass(r: Result<i32, u8>) => Result<i32, u8> { v := r?; Result<i32, u8>::Ok { value = v; } }
         main() => void { }",
    );
    workspace.patch_core("result.omg", "exposed error: E;", "exposed reason: E;");
    assert!(
        workspace
            .unavailable(CoreFeature::TryOperator)
            .contains("exactly 'Ok { value }' and 'Err { error }'")
    );
}

#[test]
fn typeinfo_checks_the_field_types_the_table_is_built_with() {
    let workspace =
        Workspace::new("helper() => usize { typeinfo<i32>.size }\nmain() => void { helper(); }");
    workspace.patch_core(
        "reflection.omg",
        "exposed size: usize;",
        "exposed size: u32;",
    );
    assert_eq!(
        workspace.unavailable(CoreFeature::Reflection),
        "'core::reflection::TypeInfo::size' must be 'usize', found 'u32'"
    );
}

#[test]
fn a_loop_uses_core_iterator_never_a_program_spec_of_that_name() {
    let errors = Workspace::new(
        "spec Iterator<T> { next(^self) => Option<T>; }
         struct Countdown { exposed left: i32; }
         meet Iterator<i32> for Countdown {
             next(^self) => Option<i32> { Option<i32>::None }
         }
         helper() => void {
             ^c := Countdown { left = 3; };
             for item in c { }
         }
         main() => void { helper(); }",
    )
    .errors();
    assert!(
        errors.iter().any(|error| matches!(
            error,
            CompileError::Analysis { errors, .. } if errors.iter().any(|error| matches!(
                error.kind,
                AnalysisErrorKind::ForLoopSourceNotIterable { .. }
            ))
        )),
        "{errors:#?}"
    );
}
