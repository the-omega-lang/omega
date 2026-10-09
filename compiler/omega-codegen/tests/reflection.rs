//! Black-box coverage of `core::reflection` emission: a program that never
//! names `typeinfo` emits no tables, `typeinfo` itself is never a function,
//! and the tables it does emit are weak, hidden constants named by the
//! described type.

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
            "omega_codegen_reflection_test_{}_{}",
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
    ir_for_sources(&[("main.omg", source)], target)
}

fn ir_for_sources(sources: &[(&str, &str)], target: Target) -> String {
    let package = TestPackage::new(sources[0].1);
    for (path, source) in &sources[1..] {
        fs::write(package.0.join(path), source).expect("write source module");
    }
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

fn reflection_globals(ir: &str) -> Vec<&str> {
    ir.lines()
        .filter(|line| line.starts_with('@') && line.contains("typeinfo"))
        .collect()
}

#[test]
fn a_program_without_typeinfo_emits_no_tables() {
    let ir = ir_for(
        "\
struct Node { exposed value: i32; exposed next: *Node; }
main() => void {
    node := Node { value = 1; next = <*Node>0; };
}
",
        HOST,
    );
    assert!(!ir.contains("typeinfo"), "{ir}");
}

#[test]
fn runtime_use_emits_weak_hidden_constant_tables_through_cycles() {
    let ir = ir_for(
        "\
struct Node { exposed value: i32; exposed next: *Node; }
enum Light { Off, On; }
main() => void {
    node := typeinfo<Node>;
    light := typeinfo<Light>;
}
",
        HOST,
    );
    let globals = reflection_globals(&ir);
    assert!(!globals.is_empty(), "{ir}");
    for global in &globals {
        assert!(
            global.contains("weak_odr hidden constant"),
            "reflection data is compiler-generated and always hidden:\n{global}"
        );
    }
    let named = |needle: &str| {
        globals
            .iter()
            .filter(|line| {
                line.split(' ')
                    .next()
                    .is_some_and(|name| name.contains(needle))
            })
            .count()
    };
    assert_eq!(
        named("NvNtC4main4Node8typeinfo"),
        1,
        "one Node table:\n{ir}"
    );
    assert_eq!(
        named("NvXPNtC4main4Node8typeinfo"),
        1,
        "one *Node table:\n{ir}"
    );
    assert_eq!(
        named("prototype"),
        2,
        "one prototype per Light variant:\n{ir}"
    );
    assert!(!ir.contains(".placeholder"), "{ir}");
    assert!(
        !ir.lines()
            .any(|line| line.starts_with("define") && line.contains("typeinfo")),
        "typeinfo is an operator, not a function:\n{ir}"
    );
}

#[test]
fn a_table_held_through_a_byte_pointer_is_still_emitted() {
    let ir = ir_for(
        "\
struct Node { exposed value: i32; }
comp NODE := <*u8>typeinfo<Node>;
main() => void {
    node := NODE;
}
",
        HOST,
    );
    let globals = reflection_globals(&ir);
    assert!(
        globals
            .iter()
            .any(|line| line.starts_with("@_omg_NvNtC4main4Node8typeinfo ")),
        "{ir}"
    );
}

#[test]
fn function_types_differing_only_in_descriptors_share_one_table() {
    let ir = ir_for(
        "\
struct Callbacks {
    exposed first: (x: i32) => void;
    exposed second: (y: i32) => void;
}

main() => void {
    callbacks := typeinfo<Callbacks>;
}
",
        HOST,
    );
    assert!(ir.contains("(i32) => void"), "{ir}");
    assert!(
        !ir.contains("(x: i32)") && !ir.contains("(y: i32)"),
        "a descriptor reached a table's initializer:\n{ir}"
    );
}

#[test]
fn anonymous_enum_member_names_agree_across_emission_units() {
    let ir = ir_for_sources(
        &[
            (
                "main.omg",
                "import self::other;
main() => void {
    local := typeinfo<enum (x: i32) => void | u8>;
    remote := other::info();
}",
            ),
            (
                "other.omg",
                "exposed info() => *core::reflection::TypeInfo {
    typeinfo<enum (y: i32) => void | u8>
}",
            ),
        ],
        HOST,
    );
    assert!(ir.contains("(i32) => void"), "{ir}");
    assert!(
        !ir.contains("(x: i32)") && !ir.contains("(y: i32)"),
        "an anonymous-enum member descriptor reached a table's initializer:\n{ir}"
    );
}
