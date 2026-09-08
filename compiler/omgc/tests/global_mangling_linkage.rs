//! A global's selected linker name is the whole contract between separately
//! compiled Omega packages and the C that links with them: the producer's
//! definition and the consumer's foreign declaration meet at that name alone,
//! under different Omega source names on each side.
//!
//! The same objects also carry the binding/visibility the symbol policy chose,
//! which only the emitted ELF shows.
//!
//! This drives the real system toolchain (`cc`); if it is missing the case
//! reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// Defines the storage. `forced_value` and `plain_value` are Omega names; the
/// annotations are what decide the two symbols the linker sees.
const PRODUCER: &str = "\
@symbol(name = \"gm_forced_value\")\n\
exposed mut forced_value : i32 = 10;\n\
\n\
@symbol(mangle = disabled)\n\
exposed mut plain_value : i32 = 20;\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_producer_forced_address() => *i32 { &forced_value }\n\
\n\
@symbol(name = \"gm_exported_value\", export)\n\
exposed mut exported_value : i32 = 30;\n\
\n\
@symbol(name = \"gm_exported_function\", export)\n\
exposed exported_function() => i32 { exported_value }\n";

/// A separate compilation that owns none of that storage: it reaches both
/// globals through foreign data bindings whose Omega names differ from the
/// producer's.
const CONSUMER: &str = "\
@symbol(name = \"gm_forced_value\")\n\
foreign renamed_forced : i32;\n\
\n\
@symbol(name = \"plain_value\")\n\
foreign renamed_plain : i32;\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_forced_value() => i32 { renamed_forced }\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_plain_value() => i32 { renamed_plain }\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_forced_address() => *i32 { &renamed_forced }\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_plain_address() => *i32 { &renamed_plain }\n";

const HARNESS: &str = "\
extern int gm_forced_value;\n\
extern int plain_value;\n\
int omega_forced_value(void);\n\
int omega_plain_value(void);\n\
const int *omega_forced_address(void);\n\
const int *omega_plain_address(void);\n\
const int *omega_producer_forced_address(void);\n\
extern int gm_exported_value;\n\
int gm_exported_function(void);\n\
\n\
int main(void) {\n\
    if (gm_forced_value != 10) return 1;\n\
    if (plain_value != 20) return 2;\n\
    if (omega_forced_value() != 10) return 3;\n\
    if (omega_plain_value() != 20) return 4;\n\
    if (omega_forced_address() != &gm_forced_value) return 5;\n\
    if (omega_plain_address() != &plain_value) return 6;\n\
    if (omega_producer_forced_address() != &gm_forced_value) return 7;\n\
    gm_forced_value = 33;\n\
    if (omega_forced_value() != 33) return 8;\n\
    if (gm_exported_value != 30) return 9;\n\
    if (gm_exported_function() != 30) return 10;\n\
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
            "omgc_global_mangling_linkage_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("producer")).expect("create producer package");
        fs::create_dir_all(dir.join("consumer")).expect("create consumer package");
        fs::write(dir.join("producer/producer.omg"), PRODUCER).expect("write producer");
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
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn separately_compiled_packages_meet_on_a_globals_selected_symbol() {
    if !tool_available("cc") {
        eprintln!("skipping: this case needs the system 'cc'");
        return;
    }

    let workspace = Workspace::new();
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &["producer", "-o", "producer-objects"],
    );
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &["consumer", "-o", "consumer-objects"],
    );

    workspace.expect_ok(
        "cc",
        &[
            "harness.c",
            "producer-objects/producer.o",
            "consumer-objects/consumer.o",
            "-o",
            "harness",
        ],
    );

    let run = workspace.run("./harness", &[]);
    assert!(
        run.status.success(),
        "the producer's definition and the consumer's foreign declaration must \
         name one object; check {:?}",
        run.status.code()
    );

    if !tool_available("readelf") {
        eprintln!("skipping the symbol-table assertions: this needs 'readelf'");
        return;
    }
    let symbols = String::from_utf8_lossy(
        &workspace
            .expect_ok("readelf", &["-sW", "producer-objects/producer.o"])
            .stdout,
    )
    .into_owned();

    for (symbol, kind, visibility) in [
        ("gm_forced_value", "OBJECT", "HIDDEN"),
        ("plain_value", "OBJECT", "HIDDEN"),
        ("omega_producer_forced_address", "FUNC", "HIDDEN"),
        ("gm_exported_value", "OBJECT", "DEFAULT"),
        ("gm_exported_function", "FUNC", "DEFAULT"),
    ] {
        let entry = symbol_entry(&symbols, symbol);
        assert!(
            entry.contains(kind) && entry.contains(" GLOBAL ") && entry.contains(visibility),
            "'{symbol}' must be a GLOBAL {visibility} {kind}, got:\n{entry}"
        );
    }
}

/// The `readelf -sW` line whose name column is exactly `symbol`.
fn symbol_entry<'a>(symbols: &'a str, symbol: &str) -> &'a str {
    symbols
        .lines()
        .find(|line| line.split_whitespace().next_back() == Some(symbol))
        .unwrap_or_else(|| panic!("no symbol-table entry for '{symbol}' in:\n{symbols}"))
}
