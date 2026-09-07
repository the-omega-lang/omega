//! `@layout(align)` is an address guarantee across separate compilation and
//! across optimization levels: two independently compiled sources must agree
//! on an aggregate's size, field offsets and flattened call signature, a
//! shared global must be defined at its type's alignment, and a constant both
//! objects reference must merge into one compatible weak definition.
//!
//! LLVM verification alone would not catch a disagreement here, so this drives
//! the real system toolchain and compares executable results. If `cc` is
//! missing the case reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

const PRODUCER: &str = "\
@layout(align = 64)\n\
exposed struct Wide { exposed value: i64; }\n\
\n\
exposed struct Holder {\n\
    exposed lead: u8;\n\
    exposed inner: Wide;\n\
    exposed trail: u8;\n\
}\n\
\n\
exposed mut SHARED: Holder;\n\
\n\
exposed comp WIDE_CONST := Wide { value = 7i64; };\n\
\n\
exposed make(value: i64) => Holder {\n\
    Holder { lead = 1u8; inner = Wide { value = value; }; trail = 2u8; }\n\
}\n\
\n\
exposed sum(value: Holder) => i64 {\n\
    value.inner.value + <i64>value.lead + <i64>value.trail\n\
}\n\
\n\
exposed constant_address() => usize { <usize>&WIDE_CONST }\n";

/// Compiled into its own object: every layout fact it uses about `Holder`
/// comes from the shared semantic layout, not from the producer's emission.
const CONSUMER: &str = "\
import root::producer;\n\
\n\
@mangling(disabled)\n\
exposed omega_roundtrip(value: i64) => i64 {\n\
    producer::sum(producer::make(value))\n\
}\n\
\n\
@mangling(disabled)\n\
exposed omega_shared_address(value: i64) => usize {\n\
    producer::SHARED = producer::make(value);\n\
    <usize>&producer::SHARED.inner\n\
}\n\
\n\
@mangling(disabled)\n\
exposed omega_shared_value() => i64 { producer::SHARED.inner.value }\n\
\n\
@mangling(disabled)\n\
exposed omega_producer_constant() => usize { producer::constant_address() }\n\
\n\
@mangling(disabled)\n\
exposed omega_consumer_constant() => usize { <usize>&producer::WIDE_CONST }\n\
\n\
@mangling(disabled)\n\
exposed omega_constant_value() => i64 { producer::WIDE_CONST.value }\n";

const MAIN: &str = "\
long omega_roundtrip(long value);\n\
unsigned long omega_shared_address(long value);\n\
long omega_shared_value(void);\n\
unsigned long omega_producer_constant(void);\n\
unsigned long omega_consumer_constant(void);\n\
long omega_constant_value(void);\n\
\n\
int main(void) {\n\
    if (omega_roundtrip(40) != 43) return 1;\n\
    if (omega_shared_address(11) % 64 != 0) return 2;\n\
    if (omega_shared_value() != 11) return 3;\n\
    if (omega_producer_constant() % 64 != 0) return 4;\n\
    if (omega_consumer_constant() % 64 != 0) return 5;\n\
    if (omega_producer_constant() != omega_consumer_constant()) return 6;\n\
    if (omega_constant_value() != 7) return 7;\n\
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
            "omgc_aligned_linkage_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("lib")).expect("create package root");
        fs::write(dir.join("lib/lib.omg"), "").expect("write package root module");
        fs::write(dir.join("lib/producer.omg"), PRODUCER).expect("write producer");
        fs::write(dir.join("lib/consumer.omg"), CONSUMER).expect("write consumer");
        fs::write(dir.join("main.c"), MAIN).expect("write consumer main");
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
fn aligned_aggregates_agree_across_objects_at_every_optimization_level() {
    if !tool_available("cc") {
        eprintln!("skipping: this case needs the system 'cc'");
        return;
    }

    for level in ["-O0", "-O2", "-O3"] {
        let workspace = Workspace::new();
        // Every participating object is rebuilt at the same level: mixing
        // old and new layouts is not a supported configuration.
        workspace.expect_ok(env!("CARGO_BIN_EXE_omgc"), &["lib", "-o", "objects", level]);
        workspace.expect_ok(
            "cc",
            &[
                "main.c",
                "objects/producer.o",
                "objects/consumer.o",
                "-o",
                "program",
            ],
        );

        let run = workspace.run("./program", &[]);
        assert!(
            run.status.success(),
            "{level}: the aligned cross-object program failed with {:?}",
            run.status.code()
        );
    }
}
