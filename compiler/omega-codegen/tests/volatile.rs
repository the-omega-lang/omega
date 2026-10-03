//! Black-box coverage of the compiler-implemented `core::volatile` bodies:
//! each instance is one volatile access per leaf, with the alignment
//! `layout::volatile_alignment` assigns, through the real driver/MIR pipeline
//! down to textual LLVM IR.

use omega_analyzer::{Arch, Os, Target};
use omega_driver::{Driver, ExternRoot};
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
            "omega_codegen_volatile_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
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

fn ir_for(source: &str, target: Target) -> String {
    let package = TestPackage::new(source);
    let program = match Driver::new(
        package.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core_root(),
        }],
        target,
    )
    .expect("construct driver with the real core extern")
    .compile(&[Ident("main".to_string())])
    {
        Ok(program) => program,
        Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
    };
    let extern_functions = program.extern_functions.clone();
    let entry = program.entry.clone();
    let sources = program
        .sources
        .iter()
        .map(|source| omega_mir::EmissionSource {
            module: source.module.clone(),
            path: source.relative_path.clone(),
        })
        .collect::<Vec<_>>();
    let modules = omega_mir::lower_program(program.modules, &entry);
    omega_codegen::generate(omega_codegen::CodegenRequest {
        target,
        opt_level: omega_codegen::OptLevel::O0,
        emit: omega_codegen::EmitKind::Ir,
        units: omega_mir::plan_emission(modules, &sources),
        entry,
        extern_functions,
    })
    .expect("codegen succeeds")
    .into_iter()
    .map(|artifact| match artifact.output {
        omega_codegen::EmitOutput::Text(text) => text,
        omega_codegen::EmitOutput::Object(_) => unreachable!("EmitKind::Ir emits text"),
    })
    .collect::<Vec<_>>()
    .join("\n")
}

const HOST: Target = Target {
    arch: Arch::X86_64,
    os: Os::Linux,
};

fn volatile_lines<'a>(ir: &'a str, keyword: &str) -> Vec<&'a str> {
    ir.lines()
        .filter(|line| line.contains(&format!("{keyword} volatile")))
        .collect()
}

fn calls_to(ir: &str, function: &str) -> usize {
    ir.lines()
        .filter(|line| line.contains("call ") && line.contains(function))
        .count()
}

#[test]
fn scalar_accesses_are_volatile_and_naturally_aligned() {
    let ir = ir_for(
        "\
import core::volatile::read_volatile;
import core::volatile::write_volatile;
main() => void {
    mut register := 0u32;
    write_volatile(&mut register, 1u32);
    write_volatile(&mut register, 2u32);
    first := read_volatile(&register);
    second := read_volatile(&register);
}
",
        HOST,
    );

    assert_eq!(calls_to(&ir, "read_volatile"), 2, "{ir}");
    assert_eq!(calls_to(&ir, "write_volatile"), 2, "{ir}");
    let loads = volatile_lines(&ir, "load");
    let stores = volatile_lines(&ir, "store");
    assert_eq!(loads.len(), 1, "one load in the u32 instance:\n{ir}");
    assert_eq!(stores.len(), 1, "one store in the u32 instance:\n{ir}");
    assert!(
        loads[0].contains("i32") && loads[0].ends_with("align 4"),
        "{ir}"
    );
    assert!(
        stores[0].contains("i32") && stores[0].ends_with("align 4"),
        "{ir}"
    );
}

#[test]
fn packed_struct_access_keeps_its_declared_alignment() {
    let ir = ir_for(
        "\
import core::volatile::write_volatile;
struct Packed { exposed word: u32; exposed byte: u8; }
main() => void {
    mut slot := Packed { word = 0u32; byte = 0u8; };
    write_volatile(&mut slot, Packed { word = 1u32; byte = 2u8; });
}
",
        HOST,
    );

    let stores = volatile_lines(&ir, "store");
    assert_eq!(stores.len(), 2, "one store per leaf:\n{ir}");
    assert!(stores.iter().all(|line| line.ends_with("align 1")), "{ir}");
}

#[test]
fn zero_sized_access_emits_nothing() {
    let ir = ir_for(
        "\
import core::volatile::read_volatile;
import core::volatile::write_volatile;
marker Token { }
main() => void {
    mut token := Token { };
    write_volatile(&mut token, Token { });
    <void>read_volatile(&token);
}
",
        HOST,
    );

    assert_eq!(calls_to(&ir, "write_volatile"), 1, "{ir}");
    assert!(
        !ir.contains("volatile i") && !ir.contains("volatile ptr"),
        "{ir}"
    );
    assert!(volatile_lines(&ir, "load").is_empty(), "{ir}");
    assert!(volatile_lines(&ir, "store").is_empty(), "{ir}");
}

#[test]
fn pointer_sized_access_follows_a_narrow_pointer_target() {
    let source = "\
import core::volatile::read_volatile;
main() => void {
    size := 1usize;
    word := 2u32;
    <void>read_volatile(&size);
    <void>read_volatile(&word);
}
";
    for target in [
        Target {
            arch: Arch::Avr,
            os: Os::None,
        },
        Target {
            arch: Arch::Riscv32,
            os: Os::None,
        },
    ] {
        let ir = ir_for(source, target);
        let pointer_bytes = if target.arch == Arch::Avr { 2 } else { 4 };
        let loads = volatile_lines(&ir, "load");
        assert_eq!(loads.len(), 2, "{target:?}:\n{ir}");
        assert!(
            loads
                .iter()
                .any(|line| line.ends_with(&format!("align {pointer_bytes}"))),
            "{target:?} usize must use the pointer width:\n{ir}"
        );
        assert!(
            loads
                .iter()
                .any(|line| line.contains("i32") && line.ends_with("align 4")),
            "{target:?} u32 stays at 4:\n{ir}"
        );
    }
}
