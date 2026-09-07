//! A global's selected linker name is the whole contract between separately
//! compiled Omega packages and the C that links with them: the producer's
//! definition and the consumer's foreign declaration meet at that name alone,
//! under different Omega source names on each side.
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
@mangling(force = \"gm_forced_value\")\n\
exposed mut forced_value : i32 = 10;\n\
\n\
@mangling(disabled)\n\
exposed mut plain_value : i32 = 20;\n\
\n\
@mangling(disabled)\n\
exposed omega_producer_forced_address() => *i32 { &forced_value }\n";

/// A separate compilation that owns none of that storage: it reaches both
/// globals through foreign data bindings whose Omega names differ from the
/// producer's.
const CONSUMER: &str = "\
@mangling(force = \"gm_forced_value\")\n\
foreign renamed_forced : i32;\n\
\n\
@mangling(force = \"plain_value\")\n\
foreign renamed_plain : i32;\n\
\n\
@mangling(disabled)\n\
exposed omega_forced_value() => i32 { renamed_forced }\n\
\n\
@mangling(disabled)\n\
exposed omega_plain_value() => i32 { renamed_plain }\n\
\n\
@mangling(disabled)\n\
exposed omega_forced_address() => *i32 { &renamed_forced }\n\
\n\
@mangling(disabled)\n\
exposed omega_plain_address() => *i32 { &renamed_plain }\n";

const HARNESS: &str = "\
extern int gm_forced_value;\n\
extern int plain_value;\n\
int omega_forced_value(void);\n\
int omega_plain_value(void);\n\
const int *omega_forced_address(void);\n\
const int *omega_plain_address(void);\n\
const int *omega_producer_forced_address(void);\n\
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
}
