//! Black-box coverage of the one-source/one-object contract: a package's
//! emitted artifacts must correspond exactly to its physical `.omg` files,
//! cross-source references must stay declarations, and symbol collisions must
//! still be rejected once the definitions live in different objects. The
//! `@symbol` visibility decisions are checked here too, because only the
//! emitted IR shows what a definition and every reference to it agreed on.
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
    generate_at(files, omega_codegen::OptLevel::O0)
}

fn generate_at(
    files: &[(&str, &str)],
    opt_level: omega_codegen::OptLevel,
) -> Result<Vec<(String, String)>, String> {
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
        opt_level,
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
        user.contains(&format!("@{global} = external hidden global")),
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
            "@symbol(name = \"collide\")\nmain() => void { }\n",
        ),
        (
            "other.omg",
            "@symbol(name = \"collide\")\nexposed other() => void { }\n",
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
            "import root::owner;\nmain() => void { owner::bump(owner::TOTAL); }\n",
        ),
        (
            "owner.omg",
            "@symbol(name = \"unmangled_symbol_with_default_value\")\n\
             exposed mut TOTAL: i32 = 10;\n\
             exposed bump(amount: i32) => void { TOTAL += amount; }\n",
        ),
    ]);
    let owner = ir_of(&artifacts, "owner.omg");
    let user = ir_of(&artifacts, "main.omg");

    assert!(
        owner.contains(
            r#"@unmangled_symbol_with_default_value = hidden global [4 x i8] c"\0A\00\00\00""#
        ),
        "the owning source defines the forced symbol with its initializer:\n{owner}"
    );
    assert!(
        !owner.contains("@main.owner.TOTAL"),
        "the module-qualified name must not also be emitted:\n{owner}"
    );
    assert!(
        user.contains("@unmangled_symbol_with_default_value = external hidden global"),
        "a non-owning source references the same selected name:\n{user}"
    );
    for line in user.lines() {
        assert!(
            !line.starts_with("@unmangled_symbol_with_default_value = hidden global "),
            "the forced global must have exactly one definition:\n{user}"
        );
    }
}

#[test]
fn a_disabled_global_uses_its_written_identifier() {
    let artifacts = artifacts(&[(
        "main.omg",
        "@symbol(mangle = disabled)\nmut plain_flag: i32 = 3;\nmain() => void { plain_flag += 1; }\n",
    )]);

    assert!(
        ir_of(&artifacts, "main.omg")
            .contains(r#"@plain_flag = hidden global [4 x i8] c"\03\00\00\00""#),
        "{}",
        ir_of(&artifacts, "main.omg")
    );
}

#[test]
fn globals_colliding_across_sources_are_rejected_by_their_symbol() {
    let cases = [
        (
            "another global",
            "@symbol(name = \"collide\")\nexposed other: i32 = 2;\n",
        ),
        (
            "a function",
            "@symbol(name = \"collide\")\nexposed other() => void { }\n",
        ),
        (
            "a foreign data binding",
            "@symbol(name = \"collide\")\nexposed foreign other: i32;\n",
        ),
    ];

    for (what, other) in cases {
        let error = generate(&[
            (
                "main.omg",
                "@symbol(name = \"collide\")\nvalue: i32 = 1;\nmain() => void { }\n",
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

/// Sources exercising every shape that carries a symbol policy: ordinary and
/// exported functions and storage, methods, foreign declarations and a foreign
/// definition, an uncalled exported definition, weak generic instantiations, a
/// gap/glue pair, a vtable, and constant backing data. Hidden is the default an
/// Omega-declared item gets; a `foreign` item's default is the opposite,
/// because declaring one is already a statement about crossing a boundary.
const VISIBILITY_OWNER: &str = "\
gap Capability {\n\
    provide(value: i32) => i32;\n\
}\n\
\n\
exposed mut TOTAL: i32 = 0;\n\
exposed bump(amount: i32) => void { TOTAL += amount; }\n\
\n\
@symbol(export)\n\
exposed exported_fn(x: i32) => i32 { x }\n\
\n\
@symbol(name = \"uncalled_exported\", export)\n\
exposed uncalled(x: i32) => i32 { x }\n\
\n\
@symbol(export = disabled)\n\
exposed explicitly_hidden(x: i32) => i32 { x }\n\
\n\
@symbol(export)\n\
exposed mut EXPORTED_TOTAL: i32 = 1;\n\
\n\
exposed foreign imported_data : i32;\n\
\n\
@symbol(export = disabled)\n\
exposed foreign in_image_data : i32;\n\
\n\
exposed foreign(c) imported_fn(x: i32) => i32;\n\
\n\
@symbol(export = disabled)\n\
exposed foreign(c) in_image_foreign_fn(x: i32) => i32;\n\
\n\
exposed foreign(c) foreign_definition(x: i32) => i32 { x + 1 }\n\
\n\
exposed struct Counter {\n\
    exposed value: i32;\n\
\n\
    @symbol(export)\n\
    exposed bump_it(*mut self, amount: i32) => void { self.value += amount; }\n\
\n\
    exposed hidden_method(*self) => i32 { self.value }\n\
}\n\
\n\
exposed greet() => *str {\n\
    <void>Capability::provide(1);\n\
    mut counter := Counter { value = 0; };\n\
    counter.bump_it(1);\n\
    <void>counter.hidden_method();\n\
    <void>imported_data;\n\
    <void>in_image_data;\n\
    <void>imported_fn(1);\n\
    <void>in_image_foreign_fn(2);\n\
    <void>foreign_definition(3);\n\
    \"constant backing data\"\n\
}\n";

const VISIBILITY_USER: &str = "\
import root::owner;\n\
\n\
glue root::owner::Capability {\n\
    provide(value: i32) => i32 { value + 1 }\n\
}\n\
\n\
exposed spec Describe {\n\
    describe(*self) => i32;\n\
}\n\
\n\
struct Thing {\n\
    exposed value: i32;\n\
}\n\
\n\
meet Describe for Thing {\n\
    describe(*self) => i32 { self.value }\n\
}\n\
\n\
hidden_generic<T>(value: T) => T { value }\n\
\n\
@symbol(export)\n\
exported_generic<T>(value: T) => T { value }\n\
\n\
describe_any(item: *spec Describe) => i32 { item.describe() }\n\
\n\
main() => void {\n\
    owner::bump(owner::TOTAL);\n\
    owner::EXPORTED_TOTAL += owner::exported_fn(1);\n\
    <void>owner::explicitly_hidden(2);\n\
    <void>hidden_generic(1);\n\
    <void>exported_generic(2);\n\
    thing := Thing { value = 3; };\n\
    <void>describe_any(&thing);\n\
    <void>owner::greet();\n\
}\n";

/// Every `@` definition/declaration line naming `symbol`, so an assertion sees
/// the full `= <linkage> <visibility>` / `define <linkage> <visibility>` text.
fn lines_naming<'a>(ir: &'a str, symbol: &str) -> Vec<&'a str> {
    ir.lines()
        .filter(|line| {
            (line.starts_with('@') || line.starts_with("define ") || line.starts_with("declare "))
                && line.contains(&format!("@{symbol}"))
        })
        .collect()
}

fn only_line<'a>(ir: &'a str, symbol: &str) -> &'a str {
    let lines = lines_naming(ir, symbol);
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one top-level line naming '{symbol}', got {lines:#?}"
    );
    lines[0]
}

#[test]
fn hidden_is_the_default_and_export_selects_llvm_default_visibility() {
    let artifacts = artifacts(&[
        ("main.omg", VISIBILITY_USER),
        ("owner.omg", VISIBILITY_OWNER),
    ]);
    let owner = ir_of(&artifacts, "owner.omg");

    let total = symbol_containing(owner, "5TOTAL");
    let bump = symbol_containing(owner, "4bump");
    let exported_fn = symbol_containing(owner, "exported_fn");
    let hidden_method = symbol_containing(owner, "hidden_method");
    let exported_method = symbol_containing(owner, "bump_it");
    let explicitly_hidden = symbol_containing(owner, "explicitly_hidden");
    let exported_total = symbol_containing(owner, "EXPORTED_TOTAL");

    for (symbol, description) in [
        (bump.as_str(), "an ordinary function"),
        (hidden_method.as_str(), "an ordinary method"),
        (
            explicitly_hidden.as_str(),
            "a function writing 'export = disabled' out",
        ),
        (total.as_str(), "ordinary storage"),
        (
            "in_image_foreign_fn",
            "a foreign declaration that opts back into this image",
        ),
        (
            "in_image_data",
            "a foreign data binding that opts back into this image",
        ),
    ] {
        assert!(
            only_line(owner, symbol).contains(" hidden "),
            "{description} defaults to hidden:\n{}",
            only_line(owner, symbol)
        );
    }

    for (symbol, description) in [
        (exported_fn.as_str(), "an exported function"),
        (exported_method.as_str(), "an exported method"),
        (exported_total.as_str(), "exported storage"),
        ("uncalled_exported", "an uncalled exported definition"),
        // A `foreign` item already declares that its symbol is looked up in,
        // or defined for, something outside this compilation, so it needs no
        // `export` to cross an image.
        ("foreign_definition", "a foreign definition"),
        ("imported_fn", "a foreign function declaration"),
        ("imported_data", "a foreign data binding"),
    ] {
        assert!(
            !only_line(owner, symbol).contains(" hidden "),
            "{description} is emitted with LLVM default visibility:\n{}",
            only_line(owner, symbol)
        );
    }
}

/// Source visibility and naming are separate decisions: `exposed` does not
/// export, `@symbol(export)` does not widen source access, and neither changes
/// the linker name.
#[test]
fn visibility_is_independent_of_source_visibility_and_naming() {
    let artifacts = artifacts(&[
        ("main.omg", VISIBILITY_USER),
        ("owner.omg", VISIBILITY_OWNER),
    ]);
    let owner = ir_of(&artifacts, "owner.omg");

    let bump = symbol_containing(owner, "4bump");
    assert!(
        bump.starts_with("_omg_") && only_line(owner, &bump).contains(" hidden "),
        "an 'exposed' function keeps its mangled name and stays hidden:\n{}",
        only_line(owner, &bump)
    );
    assert!(
        only_line(owner, "uncalled_exported").contains("define i32 @uncalled_exported("),
        "an exact name is unaffected by 'export':\n{}",
        only_line(owner, "uncalled_exported")
    );
}

#[test]
fn a_reference_carries_the_definitions_visibility() {
    let artifacts = artifacts(&[
        ("main.omg", VISIBILITY_USER),
        ("owner.omg", VISIBILITY_OWNER),
    ]);
    let owner = ir_of(&artifacts, "owner.omg");
    let user = ir_of(&artifacts, "main.omg");

    let total = symbol_containing(owner, "5TOTAL");
    let exported_fn = symbol_containing(owner, "exported_fn");
    let bump = symbol_containing(owner, "4bump");

    assert!(
        only_line(user, &total).contains("external hidden global"),
        "a reference to hidden storage stays hidden:\n{}",
        only_line(user, &total)
    );
    assert!(
        only_line(user, &bump).contains("declare hidden "),
        "a reference to a hidden function stays hidden:\n{}",
        only_line(user, &bump)
    );
    assert!(
        !only_line(user, &exported_fn).contains(" hidden "),
        "a hidden reference must not demote an exported definition:\n{}",
        only_line(user, &exported_fn)
    );
}

/// Visibility is decided alongside linkage, never instead of it: a weak
/// instantiation stays weak whether or not it is exported, and the gap/glue
/// pair still resolves to one symbol rather than an LLVM-renamed `.1`.
#[test]
fn linkage_survives_the_visibility_decision() {
    let artifacts = artifacts(&[
        ("main.omg", VISIBILITY_USER),
        ("owner.omg", VISIBILITY_OWNER),
    ]);
    let user = ir_of(&artifacts, "main.omg");
    let owner = ir_of(&artifacts, "owner.omg");

    let hidden_generic = symbol_containing(user, "hidden_generic");
    let exported_generic = symbol_containing(user, "exported_generic");
    let vtable = symbol_containing(user, "vtable");
    let glue = symbol_containing(user, "Capability7provide");

    assert!(
        only_line(user, &hidden_generic).contains("define weak_odr hidden "),
        "a generic instance stays weak and is hidden by default:\n{}",
        only_line(user, &hidden_generic)
    );
    assert!(
        only_line(user, &exported_generic).starts_with("define weak_odr i32 "),
        "exporting an instance must not strengthen its linkage:\n{}",
        only_line(user, &exported_generic)
    );
    assert!(
        only_line(user, &vtable).contains("weak_odr hidden constant"),
        "a vtable is compiler-generated and always hidden:\n{}",
        only_line(user, &vtable)
    );
    assert!(
        only_line(user, &glue).contains("define hidden "),
        "the glue definition owns the shared gap symbol:\n{}",
        only_line(user, &glue)
    );
    assert!(
        only_line(owner, &glue).contains("declare hidden "),
        "the gap declaration in another object reuses that same symbol:\n{}",
        only_line(owner, &glue)
    );
    for (source, ir) in &artifacts {
        assert!(
            !ir.contains(&format!("@{glue}.1")),
            "the gap/glue pair must not be renamed apart in {source}:\n{ir}"
        );
    }

    let constant = owner
        .lines()
        .find(|line| line.starts_with('@') && line.contains("constant"))
        .expect("the string literal produces constant backing data");
    assert!(
        constant.contains("weak_odr hidden constant"),
        "content-addressed constant data is hidden:\n{constant}"
    );
}

/// Optimization must not drop or rewrite a visibility decision, and must not
/// discard an exported definition nothing in this compilation calls.
#[test]
fn an_optimized_build_preserves_the_same_visibility() {
    let artifacts = generate_at(
        &[
            ("main.omg", VISIBILITY_USER),
            ("owner.omg", VISIBILITY_OWNER),
        ],
        omega_codegen::OptLevel::O2,
    )
    .expect("codegen succeeds");
    let owner = ir_of(&artifacts, "owner.omg");

    let bump = symbol_containing(owner, "4bump");
    assert!(
        only_line(owner, &bump).contains(" hidden "),
        "an ordinary definition is still hidden at -O2:\n{}",
        only_line(owner, &bump)
    );
    assert!(
        !only_line(owner, "uncalled_exported").contains(" hidden "),
        "an uncalled exported definition survives -O2 with default visibility:\n{}",
        only_line(owner, "uncalled_exported")
    );
}

/// The declaration pass runs before any body exists, so it names everything the
/// catalog holds. A name this object never reached must not survive into it: a
/// hidden undefined symbol is a link requirement even with no relocation
/// against it, and an object must not demand capabilities it does not use.
#[test]
fn a_declaration_this_object_never_used_is_not_emitted() {
    let artifacts = artifacts(&[
        ("main.omg", VISIBILITY_USER),
        ("owner.omg", VISIBILITY_OWNER),
    ]);
    let user = ir_of(&artifacts, "main.omg");
    let owner = ir_of(&artifacts, "owner.omg");

    assert!(
        lines_naming(user, "uncalled_exported").is_empty(),
        "a definition nothing here calls leaves no declaration behind:\n{user}"
    );
    let hidden_method = symbol_containing(owner, "hidden_method");
    assert!(
        lines_naming(user, &hidden_method).is_empty(),
        "an unused method declaration is pruned too:\n{user}"
    );
    assert!(
        !lines_naming(owner, &symbol_containing(owner, "4bump")).is_empty(),
        "the owning object still defines it:\n{owner}"
    );
}
