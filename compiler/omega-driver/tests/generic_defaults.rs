//! A generic argument left to its declared default is completed where the
//! declaration is written, even when the gap precedes a written argument.

use omega_analyzer::Target;
use omega_driver::{Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

fn core_extern() -> ExternRoot {
    ExternRoot {
        name: Ident("core".to_string()),
        dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtime/core")
            .canonicalize()
            .expect("runtime/core exists"),
    }
}

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(modules: &[(&str, &str)]) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_generic_defaults_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        for (name, source) in modules {
            fs::write(root.join(format!("{name}.omg")), source).expect("write module");
        }
        Self(root)
    }

    fn symbols(&self) -> Vec<String> {
        let program = Driver::new(self.0.clone(), None, vec![core_extern()], Target::DEFAULT)
            .expect("construct driver")
            .compile(&omega_hir::ModulePath::new([Ident("main".into())]))
            .result
            .unwrap_or_else(|errors| panic!("expected this to compile, got: {errors:#?}"));
        let entry = program.entry.clone();
        omega_mir::lower_program(program.modules, &entry, &program.types)
            .into_iter()
            .flat_map(|(_, module)| module.items)
            .flat_map(|item| match item {
                omega_mir::MirItem::FunctionDefinition(f) => vec![f.symbol],
                omega_mir::MirItem::Struct(s) => {
                    s.functions.into_iter().map(|f| f.symbol).collect()
                }
                _ => Vec::new(),
            })
            .collect()
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

#[test]
fn a_gap_before_a_written_argument_takes_the_declaring_modules_default() {
    let package = TestPackage::new(&[
        (
            "main",
            r#"
            import self::m;
            struct Hidden { exposed wide: u64; }
            main() => void {
                a := m::width<_, u32>(1);
                b := m::explicit();
                c := Hidden { wide = 0; };
            }
            "#,
        ),
        (
            "m",
            r#"
            struct Hidden { exposed narrow: u8; }
            exposed width<T = Hidden, U = u8>(value: U) => usize {
                sizeof<T> + <usize>value
            }
            exposed explicit() => usize {
                width<Hidden, u32>(2)
            }
            "#,
        ),
    ]);
    let symbols = package.symbols();
    let width: Vec<_> = symbols.iter().filter(|s| s.contains("5width")).collect();
    // Completed at the use site, `Hidden` would name `main::Hidden` and make
    // a second instantiation.
    assert_eq!(width.len(), 1, "{width:#?}");
}
