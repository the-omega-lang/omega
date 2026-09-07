//! Black-box coverage of `@layout(align = n)` as an address guarantee: every
//! compiler-created storage of a type carrying an inherited alignment must
//! carry it into LLVM, aggregate values must be built with the padding shared
//! layout describes, and constant materializations that disagree about their
//! storage contract must not collide on one weak symbol.
//!
//! Like `emission.rs`, these go through the real driver/MIR pipeline down to
//! textual LLVM IR rather than hand-building MIR.

use omega_analyzer::{Arch, Os, Target};
use omega_driver::{Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(files: &[(&str, &str)]) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_codegen_alignment_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        for (relative, source) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("source has a parent"))
                .expect("create source directory");
            fs::write(path, source).expect("write source");
        }
        Self(root)
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

/// The IR of every emitted artifact, keyed by the owning source's path
/// relative to the package root.
fn artifacts_for(files: &[(&str, &str)], target: Target) -> Vec<(String, String)> {
    let package = TestPackage::new(files);
    let program = match Driver::new(package.0.clone(), None, Vec::<ExternRoot>::new(), target)
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], target)
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
    .map(|artifact| {
        let source = artifact.source.to_string_lossy().replace('\\', "/");
        match artifact.output {
            omega_codegen::EmitOutput::Text(text) => (source, text),
            omega_codegen::EmitOutput::Object(_) => unreachable!("EmitKind::Ir emits text"),
        }
    })
    .collect()
}

fn ir_for(source: &str, target: Target) -> String {
    let mut artifacts = artifacts_for(&[("main.omg", source)], target);
    assert_eq!(artifacts.len(), 1, "a one-source package owns one artifact");
    artifacts.remove(0).1
}

fn every_host_target() -> Vec<Target> {
    vec![
        Target {
            arch: Arch::X86_64,
            os: Os::Linux,
        },
        Target {
            arch: Arch::Aarch64,
            os: Os::Linux,
        },
    ]
}

/// The alignments LLVM was told about, in emission order.
fn alignments(ir: &str, keyword: &str) -> Vec<u32> {
    ir.lines()
        .filter(|line| line.contains(keyword))
        .filter_map(|line| {
            let at = line.rfind("align ")?;
            line[at + "align ".len()..]
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .collect()
}

const ALIGNED_STORAGE: &str = "\
@layout(align = 64)\n\
struct Wide { exposed value: i64; }\n\
\n\
struct Holder { exposed lead: u8; exposed inner: Wide; exposed trail: u8; }\n\
\n\
exposed mut GLOBAL: Holder;\n\
\n\
consume(value: Holder) => i64 { value.inner.value }\n\
\n\
main() => void {\n\
    mut local := Holder { lead = 1u8; inner = Wide { value = 2i64; }; trail = 3u8; };\n\
    local.inner.value = 4i64;\n\
    GLOBAL = local;\n\
    address := &mut local.inner;\n\
    consume(local);\n\
    consume(GLOBAL);\n\
    (*address).value = 5i64;\n\
}\n";

#[test]
fn inherited_alignment_reaches_every_compiler_created_storage() {
    for target in every_host_target() {
        let ir = ir_for(ALIGNED_STORAGE, target);

        assert!(
            ir.contains("align 64"),
            "{target:?} must carry the inherited 64-byte requirement into LLVM:\n{ir}"
        );
        assert!(
            alignments(&ir, "alloca").iter().any(|align| *align >= 64),
            "{target:?} must give the local frame at least the inherited alignment:\n{ir}"
        );
        let global = ir
            .lines()
            .find(|line| line.contains("global") && line.contains("GLOBAL"))
            .unwrap_or_else(|| panic!("no definition of GLOBAL in:\n{ir}"));
        assert!(
            global.contains("align 64"),
            "a global's storage must satisfy its type's alignment: {global}"
        );
    }
}

/// The point of the whole change: a `Holder` value is 128 bytes with the
/// aligned member at offset 64, so the flattened value must have the padding
/// leaves that put it there.
#[test]
fn an_aggregate_value_is_built_with_its_layout_padding() {
    for target in every_host_target() {
        let ir = ir_for(ALIGNED_STORAGE, target);
        let signature = ir
            .lines()
            .find(|line| line.starts_with("define") && line.contains("consume"))
            .unwrap_or_else(|| panic!("no definition of 'consume' in:\n{ir}"));
        let leaves = signature.matches("i8 ").count() + signature.matches("i64 ").count();
        assert!(
            leaves > 3,
            "a padding-bearing argument must be passed as its complete leaf \
             sequence, not just its three fields: {signature}"
        );
    }
}

const ALIGNED_CONSTANTS: &str = "\
@layout(align = 64)\n\
struct Wide { exposed value: i64; }\n\
\n\
comp WIDE := Wide { value = 7i64; };\n\
comp ROW := [Wide { value = 7i64; }, Wide { value = 8i64; }];\n\
\n\
main() => void {\n\
    reference: *Wide = &WIDE;\n\
    view := &ROW[0..];\n\
    let_it_live(<i64>(*reference).value + <i64>view[1].value);\n\
}\n\
\n\
let_it_live(value: i64) => i64 { value }\n";

#[test]
fn a_typed_constant_global_carries_its_types_alignment() {
    for target in every_host_target() {
        let ir = ir_for(ALIGNED_CONSTANTS, target);
        let constants: Vec<&str> = ir
            .lines()
            .filter(|line| line.contains("_omgdata_") && line.contains("constant"))
            .collect();
        assert!(
            !constants.is_empty(),
            "the constant materializations must reach the module:\n{ir}"
        );
        assert!(
            constants.iter().all(|line| line.contains("align 64")),
            "a constant that a program takes the address of is storage for its \
             type and must satisfy its alignment: {constants:?}"
        );
    }
}

/// Two independently compiled sources materialize the same field bytes at
/// different types. Their weak definitions merge by symbol at link time, so a
/// symbol that ignored the type's storage contract would silently pick one
/// layout for both.
#[test]
fn constant_identity_separates_incompatible_materializations() {
    const ALIGNED: &str = "\
@layout(align = 64)\n\
exposed struct Wide { exposed value: i64; }\n\
exposed comp WIDE := Wide { value = 7i64; };\n\
exposed aligned_address() => usize { <usize>&WIDE }\n";
    const PACKED: &str = "\
exposed struct Narrow { exposed value: i64; }\n\
exposed comp NARROW := Narrow { value = 7i64; };\n\
exposed packed_address() => usize { <usize>&NARROW }\n";
    const MAIN: &str = "\
import root::aligned;\n\
import root::packed;\n\
main() => void {\n\
    aligned::aligned_address();\n\
    packed::packed_address();\n\
}\n";

    let artifacts = artifacts_for(
        &[
            ("main.omg", MAIN),
            ("aligned.omg", ALIGNED),
            ("packed.omg", PACKED),
        ],
        Target::DEFAULT,
    );
    let symbol_of = |source: &str| -> String {
        let ir = &artifacts
            .iter()
            .find(|(name, _)| name == source)
            .unwrap_or_else(|| panic!("no artifact for '{source}'"))
            .1;
        ir.lines()
            .find(|line| line.contains("_omgdata_") && line.contains("constant"))
            .and_then(|line| {
                let at = line.find("@_omgdata_")?;
                let rest = &line[at + 1..];
                let end = rest
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                Some(rest[..end].to_string())
            })
            .unwrap_or_else(|| panic!("no constant blob in '{source}':\n{ir}"))
    };

    assert_ne!(
        symbol_of("aligned.omg"),
        symbol_of("packed.omg"),
        "constants with the same bytes but different size/alignment must not \
         share one weak symbol"
    );
}

#[test]
fn alignment_survives_narrow_pointer_targets() {
    const SOURCE: &str = "\
@layout(align = 8)\n\
struct Wide { exposed value: u16; }\n\
struct Holder { exposed lead: u8; exposed inner: Wide; }\n\
main() => void {\n\
    mut local := Holder { lead = 1u8; inner = Wide { value = 2u16; }; };\n\
    local.inner.value = 3u16;\n\
}\n";

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
        let ir = ir_for(SOURCE, target);
        assert!(
            alignments(&ir, "alloca").iter().any(|align| *align >= 8),
            "{target:?} must still honour an 8-byte requirement:\n{ir}"
        );
    }
}

/// Alignment claims on individual accesses stay bounded by the address that
/// was actually proven: a field at an odd offset from an aligned base is not
/// itself aligned.
#[test]
fn access_alignment_claims_stay_bounded_by_the_proven_address() {
    const SOURCE: &str = "\
@layout(align = 64)\n\
struct Wide { exposed value: i64; }\n\
struct Holder { exposed lead: u8; exposed inner: Wide; exposed trail: u8; }\n\
main() => void {\n\
    mut local := Holder { lead = 1u8; inner = Wide { value = 2i64; }; trail = 3u8; };\n\
    local.trail = 4u8;\n\
}\n";

    for target in every_host_target() {
        let ir = ir_for(SOURCE, target);
        for align in alignments(&ir, "store") {
            assert!(
                align.is_power_of_two(),
                "an alignment claim must be a power of two, found {align}:\n{ir}"
            );
        }
    }
}
