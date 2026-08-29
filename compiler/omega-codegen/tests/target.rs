//! Black-box coverage of cross-target LLVM emission: the requested Omega
//! target must reach the backend unchanged, decide pointer-sized widths, and
//! decide which LLVM address space a code pointer lives in. Like
//! `convention.rs`, these tests go through the real driver/MIR pipeline down
//! to textual LLVM IR rather than hand-building `MirModule`s.

use omega_analyzer::{Arch, Os, Target};
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
            "omega_codegen_target_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn compile(&self, target: Target) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::<ExternRoot>::new(), target)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())], target)
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn ir_for(source: &str, target: Target) -> String {
    let package = TestPackage::new(source);
    let program = match package.compile(target) {
        Ok(program) => program,
        Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
    };
    let extern_functions = program.extern_functions.clone();
    let entry = program.entry.clone();
    let modules = omega_mir::lower_program(program.modules, &entry);
    let request = omega_codegen::CodegenRequest {
        module_name: "main".to_string(),
        target,
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

fn target(arch: Arch, os: Os) -> Target {
    Target { arch, os }
}

/// Exercises `sizeof`/`usize`, a function value, a raw-pointer round trip and
/// an indirect call: every path where pointer width or a code/data pointer
/// distinction can be got wrong.
const POINTER_SMOKE: &str = "\
add(a: i32, b: i32) => i32 { a + b }\n\
apply(f: (a: i32, b: i32) => i32, x: i32) => i32 { f(x, x) }\n\
roundtrip(x: i32) => i32 {\n\
    handler : (a: i32, b: i32) => i32 = add;\n\
    address := <*void>handler;\n\
    back := <(a: i32, b: i32) => i32>address;\n\
    apply(back, x)\n\
}\n\
pointer_size() => usize { sizeof<usize> }\n";

#[test]
fn each_requested_target_reaches_llvm_as_its_own_backend_target() {
    for (arch, os, triple) in [
        (Arch::Aarch64, Os::Linux, "aarch64-unknown-linux-gnu"),
        (Arch::X86_64, Os::Windows, "x86_64-pc-windows-msvc"),
        (Arch::Avr, Os::None, "avr-unknown-unknown"),
    ] {
        let ir = ir_for(POINTER_SMOKE, target(arch, os));
        assert!(
            ir.contains(&format!("target triple = \"{triple}\"")),
            "{arch:?}/{os:?} must emit the {triple} triple:\n{ir}"
        );
        assert!(
            ir.lines().any(|line| line.starts_with("target datalayout")),
            "{arch:?}/{os:?} must install the target machine's data layout:\n{ir}"
        );
    }
}

#[test]
fn pointer_sized_values_use_the_target_pointer_width() {
    for (arch, os, size_type) in [
        (Arch::Aarch64, Os::Linux, "i64"),
        (Arch::Avr, Os::None, "i16"),
    ] {
        let ir = ir_for(POINTER_SMOKE, target(arch, os));
        let line = ir
            .lines()
            .find(|line| line.starts_with("define") && line.contains("pointer_size"))
            .unwrap_or_else(|| panic!("no definition of 'pointer_size' in:\n{ir}"));
        assert!(
            line.starts_with(&format!("define {size_type} ")),
            "`usize` must be {size_type} on {arch:?}:\n{line}"
        );
    }
}

#[test]
fn avr_puts_code_pointers_in_the_program_address_space_and_data_pointers_in_zero() {
    let ir = ir_for(POINTER_SMOKE, target(Arch::Avr, Os::None));

    let apply = ir
        .lines()
        .find(|line| line.starts_with("define") && line.contains("apply"))
        .unwrap_or_else(|| panic!("no definition of 'apply' in:\n{ir}"));
    assert!(
        apply.contains("ptr addrspace(1) %0"),
        "a function-value parameter must be a code pointer on AVR:\n{apply}"
    );
    assert!(
        ir.contains("call addrspace(1) i32 %0("),
        "an indirect call through a function value must target the program address space:\n{ir}"
    );
    assert!(
        ir.contains("addrspacecast ptr addrspace(1)") && ir.contains("addrspacecast ptr %"),
        "casting between a function value and a thin raw pointer must cross address spaces \
         in both directions:\n{ir}"
    );
}

#[test]
fn a_von_neumann_target_keeps_every_pointer_in_address_space_zero() {
    let ir = ir_for(POINTER_SMOKE, Target::DEFAULT);
    assert!(
        !ir.contains("addrspace(1)"),
        "the default target has one address space, so no code pointer may be tagged:\n{ir}"
    );
    assert!(
        !ir.contains("addrspacecast"),
        "a function-value/raw-pointer cast is a no-op where code and data share an address space:\n{ir}"
    );
}

#[test]
fn a_vtable_is_data_holding_code_pointers() {
    const DYNAMIC_DISPATCH: &str = "\
spec Counter {\n\
    value(*self) => i32;\n\
}\n\
struct Fixed {\n\
    shared n: i32;\n\
}\n\
meet Counter for Fixed {\n\
    value(*self) => i32 { self.n }\n\
}\n\
read(counter: *spec Counter) => i32 { counter.value() }\n\
total(item: *Fixed) => i32 { read(<*spec Counter>item) }\n";

    let ir = ir_for(DYNAMIC_DISPATCH, target(Arch::Avr, Os::None));

    let vtable = ir
        .lines()
        .find(|line| line.contains("vtable = "))
        .unwrap_or_else(|| panic!("no vtable global in:\n{ir}"));
    assert!(
        vtable.contains("[1 x ptr addrspace(1)]"),
        "vtable slots hold code pointers on AVR:\n{vtable}"
    );
    assert!(
        ir.contains("load ptr addrspace(1), ptr "),
        "the vtable object itself is ordinary data, so a slot is loaded out of an \
         address-space-0 pointer:\n{ir}"
    );
}
