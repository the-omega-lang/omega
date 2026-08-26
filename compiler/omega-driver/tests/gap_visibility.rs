//! Gap *functions* carry an ordinary visibility that gates who may call them
//! through a path. It is deliberately not part of the gap's ABI or glue
//! conformance identity, so an external package can still implement a
//! function it may not call.

use omega_analyzer::Target;
use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::resolver::ResolveError;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackages {
    main: PathBuf,
    lib: PathBuf,
}

impl TestPackages {
    fn new(main: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_gap_visibility_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let packages = Self {
            main: parent.join("main"),
            lib: parent.join("lib"),
        };
        fs::create_dir_all(&packages.main).expect("create main package");
        fs::create_dir_all(&packages.lib).expect("create lib package");
        fs::write(packages.main.join("main.omg"), main).expect("write root module");
        fs::write(packages.lib.join("lib.omg"), "").expect("write lib root module");
        packages
    }

    fn main_child(self, name: &str, source: &str) -> Self {
        fs::write(self.main.join(format!("{name}.omg")), source).expect("write main child");
        self
    }

    fn lib_child(self, name: &str, source: &str) -> Self {
        fs::write(self.lib.join(format!("{name}.omg")), source).expect("write lib child");
        self
    }

    fn result(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(
            self.main.clone(),
            None,
            vec![
                ExternRoot {
                    name: Ident("core".to_string()),
                    dir: core_root(),
                },
                ExternRoot {
                    name: Ident("lib".to_string()),
                    dir: self.lib.clone(),
                },
            ],
            Target::DEFAULT,
        )
        .expect("construct driver")
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

impl Drop for TestPackages {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.main.parent().expect("test root has a parent"));
    }
}

fn core_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/core")
        .canonicalize()
        .expect("runtime/core exists")
}

fn has_not_visible(errors: &[CompileError], function: &str) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| {
            matches!(
                &error.kind,
                AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible { item, .. })
                    if item.as_ref() == function
            )
        }),
        _ => false,
    })
}

const LIB_CAPABILITY: &str = "gap Capability {\n\
                                  anyone() => i32;\n\
                                  shared package_wide() => i32;\n\
                              }\n";

#[test]
fn a_gap_function_without_a_modifier_stays_callable_from_another_package() {
    TestPackages::new(
        r#"
        import lib::capability;
        glue capability::Capability {
            anyone() => i32 { 1 }
            package_wide() => i32 { 2 }
        }
        entry_fn() => i32 { capability::Capability::anyone() }
        "#,
    )
    .lib_child("capability", LIB_CAPABILITY)
    .expect_ok();
}

#[test]
fn a_shared_gap_function_is_callable_elsewhere_in_its_own_package() {
    TestPackages::new(
        r#"
        import self::caller;
        glue self::capability::Capability {
            anyone() => i32 { 1 }
            package_wide() => i32 { 2 }
        }
        entry_fn() => i32 { caller::call_it() }
        "#,
    )
    .main_child("capability", LIB_CAPABILITY)
    .main_child(
        "caller",
        "exposed call_it() => i32 { root::capability::Capability::package_wide() }\n",
    )
    .expect_ok();
}

#[test]
fn a_shared_gap_function_is_not_callable_from_another_package() {
    let errors = TestPackages::new(
        r#"
        import lib::capability;
        glue capability::Capability {
            anyone() => i32 { 1 }
            package_wide() => i32 { 2 }
        }
        entry_fn() => i32 { capability::Capability::package_wide() }
        "#,
    )
    .lib_child("capability", LIB_CAPABILITY)
    .expect_errors();

    assert!(
        has_not_visible(&errors, "package_wide"),
        "expected a visibility error for the shared gap function, got: {errors:#?}"
    );
}

#[test]
fn glue_in_another_package_still_implements_a_shared_gap_function() {
    // The previous test proves `main` cannot *call* `package_wide`. Matching
    // a glue is not a call, so `main` may still supply the function, and
    // `lib` -- which may call it -- reaches `main`'s implementation.
    TestPackages::new(
        r#"
        import lib::capability;
        import lib::inside;
        glue capability::Capability {
            anyone() => i32 { 1 }
            package_wide() => i32 { 2 }
        }
        entry_fn() => i32 { inside::use_it() }
        "#,
    )
    .lib_child("capability", LIB_CAPABILITY)
    .lib_child(
        "inside",
        "exposed use_it() => i32 { root::capability::Capability::package_wide() }\n",
    )
    .expect_ok();
}

#[test]
fn a_hidden_gap_function_is_confined_to_its_declaring_module() {
    let errors = TestPackages::new(
        r#"
        glue self::capability::Capability {
            only_here() => i32 { 1 }
        }
        entry_fn() => i32 { self::capability::Capability::only_here() }
        "#,
    )
    .main_child(
        "capability",
        "gap Capability { hidden only_here() => i32; }\n",
    )
    .expect_errors();

    assert!(
        has_not_visible(&errors, "only_here"),
        "expected a visibility error for the hidden gap function, got: {errors:#?}"
    );
}
