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
            "omega_inline_asm_test_{}_{}",
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
fn all_descriptor_forms_type_check() {
    TestPackage::new(
        "entry_fn() => void { \
            comp SIZE := 4i32; \
            mut x : i32 = 0; \
            y := 1i32; \
            asm(reg(&mut x, \"rcx\"), reg(y), const(SIZE), clobber(\"rax\")) => { nop } \
        }",
    )
    .expect_ok();
}

#[test]
fn aggregate_reg_operand_is_rejected() {
    let source = "\
        exposed struct Pair { exposed a: i32; exposed b: i32; } \
        entry_fn() => void { \
            p := Pair { a = 1; b = 2; }; \
            asm(reg(p)) => { nop } \
        }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmRegNotOneRegisterOperand { .. })
    ));
}

#[test]
fn reg_by_address_of_an_aggregate_is_still_allowed() {
    TestPackage::new(
        "exposed struct Pair { exposed a: i32; exposed b: i32; } \
         entry_fn() => void { \
            mut p := Pair { a = 1; b = 2; }; \
            asm(reg(&mut p)) => { nop } \
         }",
    )
    .expect_ok();
}

#[test]
fn const_of_a_non_comp_binding_is_rejected() {
    let source = "entry_fn() => void { \
        x := 1i32; \
        asm(const(x)) => { nop } \
    }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmConstNotComp)
    ));
}

#[test]
fn unknown_dollar_binding_is_rejected() {
    let source = "entry_fn() => void { \
        x := 1i32; \
        asm(reg(x)) => { mov eax, $missing } \
    }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmUnknownBinding { text } if text == "$missing")
    ));
}

#[test]
fn out_of_range_positional_binding_is_rejected() {
    let source = "entry_fn() => void { \
        x := 1i32; \
        asm(reg(x)) => { mov eax, $1 } \
    }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmUnknownBinding { text } if text == "$1")
    ));
}

#[test]
fn ambiguous_dollar_binding_is_rejected() {
    let source = "entry_fn() => void { \
        x := 1i32; \
        asm(reg(x), reg(x)) => { mov eax, $x } \
    }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmAmbiguousBinding { text } if text == "$x")
    ));
}

#[test]
fn positional_binding_never_reaches_a_clobber() {
    // Two bindable descriptors (reg, reg) plus a clobber: $2 must stay
    // out of range, since 'clobber' never participates in binding.
    let source = "entry_fn() => void { \
        x := 1i32; \
        y := 2i32; \
        asm(reg(x), reg(y), clobber(\"rax\")) => { mov eax, $2 } \
    }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmUnknownBinding { text } if text == "$2")
    ));
}

#[test]
fn clobber_string_is_opaque_and_never_validated_as_a_binding() {
    TestPackage::new(
        "entry_fn() => void { \
            x := 1i32; \
            asm(reg(x), clobber(\"not-a-real-register\")) => { nop } \
         }",
    )
    .expect_ok();
}
