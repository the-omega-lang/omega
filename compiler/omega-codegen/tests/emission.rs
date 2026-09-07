//! Black-box coverage of the one-source/one-object contract: a package's
//! emitted artifacts must correspond exactly to its physical `.omg` files,
//! cross-source references must stay declarations, and symbol collisions must
//! still be rejected once the definitions live in different objects.
//!
//! Like `convention.rs`, these go through the real driver/MIR pipeline down to
//! textual LLVM IR rather than hand-building MIR.

use omega_analyzer::Target;
use omega_driver::{Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    /// `files` are paths relative to the package root, so a case can describe
    /// a nested source tree as literally as it appears on disk.
    fn new(files: &[(&str, &str)]) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_codegen_emission_test_{}_{}",
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

    fn compile(&self) -> omega_driver::CompiledProgram {
        match Driver::new(
            self.0.clone(),
            None,
            Vec::<ExternRoot>::new(),
            Target::DEFAULT,
        )
        .expect("construct driver")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
        {
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

/// The IR of every emitted artifact, keyed by the owning source's path
/// relative to the package root.
fn artifacts(files: &[(&str, &str)]) -> Vec<(String, String)> {
    generate(files).expect("codegen succeeds")
}

fn generate(files: &[(&str, &str)]) -> Result<Vec<(String, String)>, String> {
    let program = TestPackage::new(files).compile();
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
    let request = omega_codegen::CodegenRequest {
        target: Target::DEFAULT,
        opt_level: omega_codegen::OptLevel::O0,
        emit: omega_codegen::EmitKind::Ir,
        units: omega_mir::plan_emission(modules, &sources),
        entry,
        extern_functions,
    };
    Ok(omega_codegen::generate(request)?
        .into_iter()
        .map(|artifact| {
            let source = artifact.source.to_string_lossy().replace('\\', "/");
            match artifact.output {
                omega_codegen::EmitOutput::Text(text) => (source, text),
                omega_codegen::EmitOutput::Object(_) => {
                    unreachable!("EmitKind::Ir always emits text")
                }
            }
        })
        .collect())
}

fn ir_of<'a>(artifacts: &'a [(String, String)], source: &str) -> &'a str {
    &artifacts
        .iter()
        .find(|(name, _)| name == source)
        .unwrap_or_else(|| panic!("no artifact for '{source}'"))
        .1
}

/// The mangled symbol of a free function or global, found by its source name.
fn symbol_containing(ir: &str, name: &str) -> String {
    ir.lines()
        .find_map(|line| {
            let at = line.find('@')?;
            let rest = &line[at + 1..];
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(rest.len());
            let symbol = &rest[..end];
            (symbol.contains(name) && !line.trim_start().starts_with(';'))
                .then(|| symbol.to_string())
        })
        .unwrap_or_else(|| panic!("no symbol mentioning '{name}' in:\n{ir}"))
}

const OWNER: &str = "\
exposed mut TOTAL: i32 = 0;\n\
exposed bump(amount: i32) => void { TOTAL += amount; }\n";

const USER: &str = "\
import root::owner;\n\
main() => void {\n\
    owner::bump(owner::TOTAL);\n\
}\n";

#[test]
fn each_source_owns_exactly_one_artifact_including_a_silent_one() {
    let artifacts = artifacts(&[
        ("main.omg", USER),
        ("owner.omg", OWNER),
        ("silent.omg", "# declares nothing\n"),
        ("nested/nested.omg", "exposed helper() => i32 { 1 }\n"),
    ]);

    assert_eq!(
        artifacts
            .iter()
            .map(|(source, _)| source.as_str())
            .collect::<Vec<_>>(),
        vec!["main.omg", "nested/nested.omg", "owner.omg", "silent.omg"],
        "one artifact per source, ordered by relative source path"
    );
    let silent = ir_of(&artifacts, "silent.omg");
    assert!(
        !silent.contains("define "),
        "a source with no definitions still owns an artifact, and defines nothing in it:\n{silent}"
    );
}

#[test]
fn a_cross_source_function_and_global_are_declared_not_redefined() {
    let artifacts = artifacts(&[("main.omg", USER), ("owner.omg", OWNER)]);
    let owner = ir_of(&artifacts, "owner.omg");
    let user = ir_of(&artifacts, "main.omg");

    let global = symbol_containing(owner, "TOTAL");
    let function = symbol_containing(owner, "bump");

    assert!(
        owner.contains(&format!("@{global} = "))
            && !owner.contains(&format!("@{global} = external")),
        "the owning source defines the global's storage:\n{owner}"
    );
    assert!(
        owner.contains("define ") && owner.contains(&format!("@{function}(")),
        "the owning source defines the function:\n{owner}"
    );

    assert!(
        user.contains(&format!("@{global} = external global")),
        "a non-owning source declares the global, never defining it again:\n{user}"
    );
    assert!(
        user.contains("declare ") && !user.contains(&format!("define {function}")),
        "a non-owning source declares the function, never defining it again:\n{user}"
    );
    for line in user.lines() {
        assert!(
            !(line.starts_with("define ") && line.contains(&format!("@{function}("))),
            "'{function}' must have exactly one definition, and it is not this object's:\n{user}"
        );
    }
}

#[test]
fn two_sources_forcing_one_linker_symbol_are_still_rejected() {
    let error = generate(&[
        (
            "main.omg",
            "@mangling(force = \"collide\")\nmain() => void { }\n",
        ),
        (
            "other.omg",
            "@mangling(force = \"collide\")\nexposed other() => void { }\n",
        ),
    ])
    .expect_err("a forced-symbol collision across sources must be rejected");

    assert!(error.contains("two different items"), "{error}");
    assert!(error.contains("collide"), "{error}");
}

/// The catalog consumes whatever name MIR settled on, so a forced global is
/// defined once under exactly that name and referenced under it everywhere
/// else.
#[test]
fn a_forced_global_is_defined_once_under_its_selected_name() {
    let artifacts = artifacts(&[
        (
            "main.omg",
            "import root::owner;\nmain() => void { owner::bump(1); }\n",
        ),
        (
            "owner.omg",
            "@mangling(force = \"unmangled_symbol_with_default_value\")\n\
             exposed mut TOTAL: i32 = 10;\n\
             exposed bump(amount: i32) => void { TOTAL += amount; }\n",
        ),
    ]);
    let owner = ir_of(&artifacts, "owner.omg");
    let user = ir_of(&artifacts, "main.omg");

    assert!(
        owner.contains(r#"@unmangled_symbol_with_default_value = global [4 x i8] c"\0A\00\00\00""#),
        "the owning source defines the forced symbol with its initializer:\n{owner}"
    );
    assert!(
        !owner.contains("@main.owner.TOTAL"),
        "the module-qualified name must not also be emitted:\n{owner}"
    );
    assert!(
        user.contains("@unmangled_symbol_with_default_value = external global"),
        "a non-owning source references the same selected name:\n{user}"
    );
    for line in user.lines() {
        assert!(
            !line.starts_with("@unmangled_symbol_with_default_value = global "),
            "the forced global must have exactly one definition:\n{user}"
        );
    }
}

#[test]
fn a_disabled_global_uses_its_written_identifier() {
    let artifacts = artifacts(&[(
        "main.omg",
        "@mangling(disabled)\nmut plain_flag: i32 = 3;\nmain() => void { plain_flag += 1; }\n",
    )]);

    assert!(
        ir_of(&artifacts, "main.omg").contains(r#"@plain_flag = global [4 x i8] c"\03\00\00\00""#),
        "{}",
        ir_of(&artifacts, "main.omg")
    );
}

#[test]
fn globals_colliding_across_sources_are_rejected_by_their_symbol() {
    let cases = [
        (
            "another global",
            "@mangling(force = \"collide\")\nexposed other: i32 = 2;\n",
        ),
        (
            "a function",
            "@mangling(force = \"collide\")\nexposed other() => void { }\n",
        ),
        (
            "a foreign data binding",
            "@mangling(force = \"collide\")\nexposed foreign other: i32;\n",
        ),
    ];

    for (what, other) in cases {
        let error = generate(&[
            (
                "main.omg",
                "@mangling(force = \"collide\")\nvalue: i32 = 1;\nmain() => void { }\n",
            ),
            ("other.omg", other),
        ])
        .unwrap_err();

        assert!(
            error.contains("two different items"),
            "colliding with {what}: {error}"
        );
        assert!(error.contains("collide"), "colliding with {what}: {error}");
    }
}
