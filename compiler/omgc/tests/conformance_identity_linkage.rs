//! A provider package conforms one type to two specs that share a name and a
//! method signature but live in different modules. A separately compiled
//! consumer calls both conformance methods (extern references to the
//! provider's definitions) and builds both spec objects (vtables emitted in
//! the consumer), so every symbol must carry the spec's full identity to bind
//! to the right definition.
//!
//! This drives the real system toolchain (`cc`, `readelf`); if `cc` is
//! missing the case reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

const SPEC: &str = "\
exposed spec P {\n\
    f(*self) => i32;\n\
}\n";

const PROVIDER: &str = "\
import self::x;\n\
import self::y;\n\
\n\
exposed struct S {\n\
    exposed tag: i32;\n\
}\n\
\n\
meet x::P for S {\n\
    f(*self) => i32 { 1 }\n\
}\n\
\n\
meet y::P for S {\n\
    f(*self) => i32 { 2 }\n\
}\n";

const CONSUMER: &str = "\
import provider;\n\
import provider::{ S, x, y };\n\
\n\
through_x(object: *spec x::P) => i32 { object.f() }\n\
\n\
through_y(object: *spec y::P) => i32 { object.f() }\n\
\n\
@symbol(mangle = disabled)\n\
exposed static_x() => i32 { s := S { tag = 0; }; <S : x::P>::f(&s) }\n\
\n\
@symbol(mangle = disabled)\n\
exposed static_y() => i32 { s := S { tag = 0; }; <S : y::P>::f(&s) }\n\
\n\
@symbol(mangle = disabled)\n\
exposed object_x() => i32 { s := S { tag = 0; }; through_x(&s) }\n\
\n\
@symbol(mangle = disabled)\n\
exposed object_y() => i32 { s := S { tag = 0; }; through_y(&s) }\n";

const HARNESS: &str = "\
int static_x(void);\n\
int static_y(void);\n\
int object_x(void);\n\
int object_y(void);\n\
\n\
int main(void) {\n\
    if (static_x() != 1) return 1;\n\
    if (static_y() != 2) return 2;\n\
    if (object_x() != 1) return 3;\n\
    if (object_y() != 2) return 4;\n\
    return 0;\n\
}\n";

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "omgc_conformance_identity_linkage_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("provider")).expect("create provider package");
        fs::create_dir_all(dir.join("consumer")).expect("create consumer package");
        fs::write(dir.join("provider/provider.omg"), PROVIDER).expect("write provider");
        fs::write(dir.join("provider/x.omg"), SPEC).expect("write provider::x");
        fs::write(dir.join("provider/y.omg"), SPEC).expect("write provider::y");
        fs::write(dir.join("consumer/consumer.omg"), CONSUMER).expect("write consumer");
        fs::write(dir.join("harness.c"), HARNESS).expect("write harness");
        Self(dir)
    }

    fn run(&self, program: &str, args: &[&str]) -> Output {
        Command::new(program)
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("run {program}: {error}"))
    }

    fn expect_ok(&self, program: &str, args: &[&str]) -> Output {
        let output = self.run(program, args);
        assert!(
            output.status.success(),
            "{program} {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// The `_omg_` symbols of `kind` (`FUNC`/`OBJECT`) an object defines, or
    /// every `_omg_` symbol it leaves undefined.
    fn mangled_symbols(&self, object: &str, defined: Option<&str>) -> Vec<String> {
        let table = String::from_utf8_lossy(&self.expect_ok("readelf", &["-sW", object]).stdout)
            .into_owned();
        let mut names: Vec<String> = table
            .lines()
            .filter(|line| match defined {
                Some(kind) => !line.contains(" UND ") && line.contains(&format!(" {kind} ")),
                None => line.contains(" UND "),
            })
            .filter_map(|line| line.split_whitespace().next_back())
            .filter(|name| name.starts_with("_omg_"))
            .map(str::to_owned)
            .collect();
        names.sort();
        names
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn same_named_specs_from_two_modules_link_to_their_own_conformances() {
    if !tool_available("cc") {
        eprintln!("skipping: this case needs the system 'cc'");
        return;
    }

    let workspace = Workspace::new();
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &["provider", "-o", "provider-objects"],
    );
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &[
            "consumer",
            "-o",
            "consumer-objects",
            "--import=provider=provider",
        ],
    );
    workspace.expect_ok(
        "cc",
        &[
            "harness.c",
            "provider-objects/provider.o",
            "provider-objects/x.o",
            "provider-objects/y.o",
            "consumer-objects/consumer.o",
            "-o",
            "harness",
        ],
    );

    let run = workspace.run("./harness", &[]);
    assert!(
        run.status.success(),
        "each call must reach its own spec's conformance; check {:?}",
        run.status.code()
    );

    if !tool_available("readelf") {
        eprintln!("skipping the symbol-table assertions: this needs 'readelf'");
        return;
    }
    let methods = workspace.mangled_symbols("provider-objects/provider.o", Some("FUNC"));
    let referenced = workspace.mangled_symbols("consumer-objects/consumer.o", None);
    let vtables = workspace.mangled_symbols("consumer-objects/consumer.o", Some("OBJECT"));
    assert_eq!(
        methods.len(),
        2,
        "the provider defines one method per conformance: {methods:?}"
    );
    assert_eq!(
        referenced, methods,
        "the consumer must reference both conformance methods byte for byte"
    );
    assert_eq!(
        vtables.len(),
        2,
        "the consumer emits one vtable per spec: {vtables:?}"
    );
}
