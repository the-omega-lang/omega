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
            "omega_function_without_body_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(
            self.0.clone(),
            None,
            vec![ExternRoot {
                name: Ident("core".to_string()),
                dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../runtime/core")
                    .canonicalize()
                    .expect("runtime/core exists"),
            }],
            Target::DEFAULT,
        )
        .expect("construct driver with the real core extern")
        .compile(&[Ident("main".to_string())])
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn bodyless_names(source: &str) -> Vec<String> {
    let errors = match TestPackage::new(source).compile() {
        Ok(_) => panic!("expected a bodyless function to be rejected"),
        Err(errors) => errors,
    };
    let mut names: Vec<String> = errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors
                .iter()
                .filter_map(|error| match &error.kind {
                    AnalysisErrorKind::FunctionWithoutBody { name } => {
                        Some(name.as_ref().to_string())
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    names.sort();
    names
}

#[test]
fn uninstantiated_generic_bodyless_function_is_rejected() {
    assert_eq!(
        bodyless_names("unfinished<T>(value: T) => T;\nmain() => void { }"),
        ["unfinished"]
    );
}

#[test]
fn concrete_bodyless_function_is_rejected_once() {
    assert_eq!(
        bodyless_names("stub() => void;\nmain() => void { }"),
        ["stub"]
    );
}

#[test]
fn the_core_volatile_declarations_compile() {
    TestPackage::new(
        "import core::volatile::read_volatile;\n\
         main() => void { x := 1u8; <void>read_volatile(&x); }",
    )
    .compile()
    .expect("the compiler-implemented declarations are accepted");
}
