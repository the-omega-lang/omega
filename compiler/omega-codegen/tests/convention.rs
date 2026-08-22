//! Black-box coverage of LLVM calling-convention propagation and C variadic
//! promotion, proven end-to-end through `omega_driver::Driver` down to
//! textual LLVM IR (`EmitKind::Ir`). Hand-constructing valid `MirModule`s
//! by hand would require reproducing `HirId`/`ResolvedType`/`CheckedParam`
//! plumbing that the driver already builds correctly; going through the
//! real pipeline keeps these tests honest about what a `.omg` source
//! snippet actually lowers to.

use omega_analyzer::Target;
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
            "omega_codegen_convention_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::<ExternRoot>::new(), Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    fn expect_ok(&self) -> omega_driver::CompiledProgram {
        match self.compile() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn ir_for(source: &str) -> String {
    let program = TestPackage::new(source).expect_ok();
    let extern_functions = program.extern_functions.clone();
    let entry = program.entry.clone();
    let modules = omega_mir::lower_program(program.modules, &entry);
    let request = omega_codegen::CodegenRequest {
        module_name: "main".to_string(),
        target: Target::DEFAULT,
        opt_level: omega_codegen::OptLevel::O0,
        emit: omega_codegen::EmitKind::Ir,
        modules,
        entry,
        extern_functions,
    };
    match omega_codegen::generate(request).expect("codegen succeeds") {
        omega_codegen::EmitOutput::Text(text) => text,
        omega_codegen::EmitOutput::Object(_) => unreachable!("EmitKind::Ir always emits text"),
    }
}

/// Locates a function's `define`/`declare` line by name. Ordinary Omega
/// functions get mangled symbols (the bare name survives as a substring of
/// the mangled one), while `foreign` functions default to their bare source
/// name, so a substring search over the whole line works for both.
fn signature_line<'a>(ir: &'a str, name: &str) -> &'a str {
    ir.lines()
        .find(|line| {
            (line.starts_with("define ") || line.starts_with("declare ")) && line.contains(name)
        })
        .unwrap_or_else(|| panic!("no line declares/defines '{name}' in:\n{ir}"))
}

// -- Function definition/declaration convention --

#[test]
fn omega_and_c_functions_share_llvm_default_calling_convention() {
    let ir = ir_for(
        "omega_fn() => void { }\n\
         foreign(c) c_fn() => void { }",
    );

    // LLVM's `ccc` (id 0) is the IR printer's default and is omitted from
    // textual output rather than spelled out explicitly.
    let omega_line = signature_line(&ir, "omega_fn");
    assert!(
        !omega_line.contains("cc"),
        "an Omega-convention function must not carry an explicit LLVM calling-convention marker:\n{omega_line}"
    );
    let c_line = signature_line(&ir, "c_fn");
    assert!(
        !c_line.contains("x86_64_sysvcc"),
        "a `foreign(c)` function must use LLVM's default C convention, not sysv64:\n{c_line}"
    );
}

#[test]
fn sysv64_function_gets_the_explicit_llvm_x86_64_sysv_convention() {
    let ir = ir_for("foreign(sysv64) sysv_fn() => void { }");

    let line = signature_line(&ir, "sysv_fn");
    assert!(
        line.contains("x86_64_sysvcc"),
        "a `foreign(sysv64)` function must carry LLVM's explicit x86_64_sysvcc marker:\n{line}"
    );
}

// -- Call-site convention --

#[test]
fn direct_call_to_a_sysv64_function_carries_the_sysv_convention_at_the_call_site() {
    let ir = ir_for(
        "foreign(sysv64) sysv_fn() => void { }\n\
         main() => void { sysv_fn(); }",
    );

    let call_line = ir
        .lines()
        .find(|line| line.contains("call") && line.contains("@sysv_fn"))
        .unwrap_or_else(|| panic!("no call site for 'sysv_fn' in:\n{ir}"));
    assert!(
        call_line.contains("x86_64_sysvcc"),
        "a direct call to a `foreign(sysv64)` function must set the sysv calling convention at the call site, not only on the declaration:\n{call_line}"
    );
}

#[test]
fn direct_call_to_a_c_function_carries_the_default_convention_at_the_call_site() {
    let ir = ir_for(
        "foreign(c) c_fn() => void { }\n\
         main() => void { c_fn(); }",
    );

    let call_line = ir
        .lines()
        .find(|line| line.contains("call") && line.contains("@c_fn"))
        .unwrap_or_else(|| panic!("no call site for 'c_fn' in:\n{ir}"));
    assert!(
        !call_line.contains("x86_64_sysvcc"),
        "a direct call to a `foreign(c)` function must not carry the sysv marker at the call site:\n{call_line}"
    );
}

// -- C variadic default-argument promotion --

#[test]
fn foreign_c_variadic_call_promotes_a_narrow_float_tail_argument() {
    let ir = ir_for(
        "foreign(c) c_variadic(fixed: i32, ...) => void;\n\
         main() => void {\n\
             x := 1.0;\n\
             c_variadic(0, x);\n\
         }",
    );

    let call_line = ir
        .lines()
        .find(|line| line.contains("call") && line.contains("@c_variadic"))
        .unwrap_or_else(|| panic!("no call site for 'c_variadic' in:\n{ir}"));
    assert!(
        call_line.contains("double"),
        "a `foreign(c)` variadic tail argument (f32) must be promoted to `double` per the C default argument promotions:\n{call_line}"
    );
    assert!(
        !call_line.contains("float "),
        "the promoted tail argument must not still appear as `float` in the call:\n{call_line}"
    );
}

#[test]
fn foreign_sysv64_variadic_call_does_not_promote_the_tail_argument() {
    let ir = ir_for(
        "foreign(sysv64) sysv_variadic(fixed: i32, ...) => void;\n\
         main() => void {\n\
             x := 1.0;\n\
             sysv_variadic(0, x);\n\
         }",
    );

    let call_line = ir
        .lines()
        .find(|line| line.contains("call") && line.contains("@sysv_variadic"))
        .unwrap_or_else(|| panic!("no call site for 'sysv_variadic' in:\n{ir}"));
    assert!(
        call_line.contains("float"),
        "a `foreign(sysv64)` variadic tail argument must keep its actual lowered Omega type (f32/`float`), not be C-promoted:\n{call_line}"
    );
}
