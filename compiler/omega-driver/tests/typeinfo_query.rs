//! Driver-level behavior of `typeinfo<Type>`: its result type is
//! `core::reflection::TypeInfo` by absolute path, so a program's own
//! `TypeInfo` never takes its place, and a compilation without `core` gets a
//! diagnostic rather than a compiler panic.

use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::{Arch, Os, Target};
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

const HOST: Target = Target {
    arch: Arch::X86_64,
    os: Os::Linux,
};

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_typeinfo_query_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn compile_with(
        &self,
        externs: Vec<ExternRoot>,
    ) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, externs, HOST)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())])
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn core() -> Vec<ExternRoot> {
    vec![ExternRoot {
        name: Ident("core".to_string()),
        dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtime/core")
            .canonicalize()
            .expect("runtime/core exists"),
    }]
}

fn analysis_errors(errors: &[CompileError]) -> Vec<&AnalysisErrorKind> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => {
                errors.iter().map(|error| &error.kind).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn typeinfo_without_core_is_a_diagnostic() {
    let package = TestPackage::new("main() => void { info := typeinfo<i32>; }");
    let errors = match package.compile_with(Vec::new()) {
        Ok(_) => panic!("typeinfo without core::reflection must be rejected"),
        Err(errors) => errors,
    };
    assert!(
        analysis_errors(&errors)
            .iter()
            .any(|kind| matches!(kind, AnalysisErrorKind::TypeinfoUnavailable)),
        "{errors:#?}"
    );
}

#[test]
fn a_local_type_info_does_not_shadow_the_table_type() {
    let package = TestPackage::new(
        "\
struct TypeInfo { exposed unrelated: u8; }
main() => void {
    mine := TypeInfo { unrelated = 1u8; };
    size: usize = typeinfo<i32>.size;
}
",
    );
    if let Err(errors) = package.compile_with(core()) {
        panic!("expected this to compile, got: {errors:#?}");
    }
}
