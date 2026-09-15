//! Black-box coverage of `omgc -D`: which declarations one invocation's
//! configuration selects, proven on the emitted artifact rather than on a
//! compiler-internal query, and how a malformed configuration is rejected.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct Workspace(PathBuf);

impl Workspace {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "omgc_definitions_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = dir.join("pkg");
        fs::create_dir_all(&root).expect("create package root");
        fs::write(root.join("pkg.omg"), source).expect("write source");
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_omgc"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .expect("run omgc")
    }

    /// Compiles to backend IR, which is where a removed declaration can be
    /// proven absent from the artifact before anything links it.
    fn ir(&self, extra: &[&str]) -> String {
        let output_dir = format!("out{}", NEXT_DIR.fetch_add(1, Ordering::Relaxed));
        let args = [&["pkg", "-o", &output_dir, "--emit=ir"], extra].concat();
        let output = self.run(&args);
        assert!(
            output.status.success(),
            "omgc {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(self.0.join(output_dir).join("pkg.ll")).expect("read emitted IR")
    }

    fn failure(&self, extra: &[&str]) -> String {
        let args = [&["pkg", "-o", "out", "--emit=ir"], extra].concat();
        let output = self.run(&args);
        assert!(
            !output.status.success(),
            "omgc {args:?} should have failed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A global's mangled symbol always contains its source name, so a name that
/// does not appear in the IR at all was never emitted.
fn defines(ir: &str, name: &str) -> bool {
    ir.lines()
        .any(|line| line.starts_with('@') && line.contains(name))
}

/// Absence is only meaningful where a boolean is expected, so a fixture
/// compiled both with and without its definitions asks about them there.
const BOOLEAN_SELECTION: &str = r#"
@cond(def::enabled)
exposed from_bare : i32 = 1;

@cond(not(def::enabled))
exposed fallback : i32 = 2;

exposed always_here : i32 = 0;
"#;

/// Every definition here is read in a value position, so each one must be
/// supplied: comparing against a definition that was never given has no
/// answer.
const SELECTION: &str = r#"
@cond(def::enabled)
exposed from_bare : i32 = 1;

@cond(equals(def::count, 123))
exposed from_number : i32 = 2;

@cond(equals(def::label, "release build"))
exposed from_string : i32 = 3;

@cond(equals(def::letter, 'x'))
exposed from_char : i32 = 4;

@cond(equals(def::ratio, 1.5))
exposed from_float : i32 = 5;

@cond(equals(def::raw, b"bytes"))
exposed from_bytes : i32 = 6;

exposed always_here : i32 = 0;
"#;

#[test]
fn every_definition_form_selects_its_declaration() {
    let workspace = Workspace::new(SELECTION);
    let ir = workspace.ir(&[
        "-Denabled",
        "-Dcount=123",
        "-Dlabel=\"release build\"",
        "-Dletter='x'",
        "-Dratio=1.5",
        "-Draw=b\"bytes\"",
    ]);
    for name in [
        "from_bare",
        "from_number",
        "from_string",
        "from_char",
        "from_float",
        "from_bytes",
        "always_here",
    ] {
        assert!(defines(&ir, name), "{name} must be emitted");
    }
}

#[test]
fn an_unselected_declaration_never_reaches_the_artifact() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    let ir = workspace.ir(&[]);
    assert!(defines(&ir, "always_here"));
    assert!(defines(&ir, "fallback"));
    assert!(
        !defines(&ir, "from_bare"),
        "an unselected declaration must not be emitted, and therefore cannot be linked"
    );
}

#[test]
fn the_separated_and_attached_forms_mean_the_same_thing() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    let attached = workspace.ir(&["-Denabled"]);
    let separated = workspace.ir(&["-D", "enabled"]);
    assert_eq!(attached, separated);
    assert!(defines(&attached, "from_bare"));
}

#[test]
fn a_repeated_definition_is_rejected_however_it_is_spelled() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    for options in [
        vec!["-Denabled", "-Denabled"],
        vec!["-Dcount=123", "-Dcount=123"],
        vec!["-Dcount=123", "-Dcount=7"],
        vec!["-Denabled", "-D", "enabled=true"],
    ] {
        assert!(
            workspace.failure(&options).contains("more than once"),
            "{options:?} must be rejected"
        );
    }
}

#[test]
fn a_malformed_definition_is_rejected_before_anything_is_compiled() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    for (options, expected) in [
        (vec!["-Dlabel=release"], "expected one literal value"),
        (vec!["-Dcount="], "needs a value"),
        (vec!["-D0abc=1"], "definition name"),
        (vec!["-Dif"], "definition name"),
        (vec!["-Dcount=300u8"], "does not fit"),
    ] {
        let message = workspace.failure(&options);
        assert!(
            message.contains(expected),
            "{options:?} should mention {expected:?}, got: {message}"
        );
    }
}

/// An unused definition is part of the configuration, so it is validated
/// against the final target whatever order the options were written in.
#[test]
fn an_unused_definition_is_validated_against_the_final_target() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    assert!(
        workspace
            .failure(&["-Dunused=65536usize", "--target=avr-none"])
            .contains("16-bit")
    );
    assert!(
        workspace
            .failure(&["--target=avr-none", "-Dunused=65536usize"])
            .contains("16-bit")
    );
    workspace.ir(&["-Dunused=65536usize", "--target=x86_64-linux"]);
}

const TARGETS: &str = r#"
@cond(equals(target_pointer_width, 16))
exposed pointer16 : i32 = 1;

@cond(equals(target_pointer_width, 32))
exposed pointer32 : i32 = 1;

@cond(equals(target_pointer_width, 64))
exposed pointer64 : i32 = 1;

@cond(target_freestanding)
exposed freestanding : i32 = 1;

@cond(not(target_freestanding))
exposed hosted : i32 = 1;

@cond(in(target_os, &["linux", "macos", "windows", "none"]))
exposed known_os : i32 = 1;

@cond(equals(target_arch, "x86"))
exposed is_x86 : i32 = 1;
"#;

#[test]
fn builtins_describe_the_selected_target_on_every_pointer_width() {
    let workspace = Workspace::new(TARGETS);
    for (target, width, freestanding) in [
        ("avr-none", "pointer16", true),
        ("thumbv7em-none", "pointer32", true),
        ("riscv64-linux", "pointer64", false),
        ("x86_64-linux", "pointer64", false),
    ] {
        let ir = workspace.ir(&[&format!("--target={target}")]);
        assert!(defines(&ir, width), "{target} must select {width}");
        assert!(defines(&ir, "known_os"), "{target} must have a known os");
        assert_eq!(
            defines(&ir, "freestanding"),
            freestanding,
            "{target} freestanding selection"
        );
        assert_eq!(defines(&ir, "hosted"), !freestanding, "{target} hosted");
    }
}

/// A CLI alias names the same target, so it selects the same declarations.
#[test]
fn canonical_target_aliases_select_the_same_declarations() {
    let workspace = Workspace::new(TARGETS);
    assert_eq!(
        workspace.ir(&["--target=i686-linux"]),
        workspace.ir(&["--target=x86-linux"])
    );
    assert_eq!(
        workspace.ir(&["--target=riscv32-freestanding"]),
        workspace.ir(&["--target=riscv32-none"])
    );
    assert!(defines(&workspace.ir(&["--target=i686-linux"]), "is_x86"));
}

/// A builtin describes the target; it is not a name a definition can take
/// over. `-Dtarget_os` defines `def::target_os`, which is a different value.
#[test]
fn a_user_definition_cannot_replace_a_builtin() {
    let workspace = Workspace::new(
        r#"
        @cond(equals(target_os, "custom"))
        exposed builtin_overridden : i32 = 1;

        @cond(equals(def::target_os, "custom"))
        exposed user_definition : i32 = 1;
        "#,
    );
    let ir = workspace.ir(&["-Dtarget_os=\"custom\""]);
    assert!(!defines(&ir, "builtin_overridden"));
    assert!(defines(&ir, "user_definition"));
}

/// Two invocations are two configurations; nothing is carried between them.
#[test]
fn separate_invocations_are_configured_separately() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    assert!(defines(&workspace.ir(&["-Denabled"]), "from_bare"));
    assert!(!defines(&workspace.ir(&[]), "from_bare"));
}

#[test]
fn the_help_text_documents_the_option() {
    let workspace = Workspace::new(BOOLEAN_SELECTION);
    let output = workspace.run(&["--help"]);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("-D<name>[=<literal>]"), "{help}");
    assert!(help.contains("def::"), "{help}");
}
