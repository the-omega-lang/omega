//! A consumer package reads and writes another package's module-level data
//! by its Omega path. The consumer owns none of that storage, so its object
//! must reference exactly the symbol each definition chose: the mangled
//! default, a forced name, a disabled-mangling name, and a `foreign` binding
//! whose storage lives in C.
//!
//! This drives the real system toolchain (`cc`); if it is missing the case
//! reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

const PROVIDER: &str = "\
exposed mut counter : i32 = 5;\n\
\n\
@symbol(name = \"egl_named\")\n\
exposed mut named : i32 = 7;\n\
\n\
@symbol(mangle = disabled)\n\
exposed mut egl_plain : i32 = 9;\n\
\n\
exposed foreign mut egl_fcounter : i32;\n\
\n\
@symbol(mangle = disabled)\n\
exposed provider_counter() => i32 { counter }\n";

const CONSUMER: &str = "\
import provider;\n\
\n\
@symbol(mangle = disabled)\n\
exposed consumer_update() => i32 {\n\
    provider::counter = provider::counter + 1;\n\
    provider::named = provider::named * 2;\n\
    provider::egl_plain += 1;\n\
    provider::egl_fcounter = provider::egl_fcounter + 100;\n\
    provider::counter + provider::named + provider::egl_plain + provider::egl_fcounter\n\
}\n";

/// `egl_fcounter` is the storage the provider's `foreign` binding names.
const HARNESS: &str = "\
int egl_fcounter = 3;\n\
extern int egl_named;\n\
extern int egl_plain;\n\
int provider_counter(void);\n\
int consumer_update(void);\n\
\n\
int main(void) {\n\
    if (consumer_update() != 6 + 14 + 10 + 103) return 1;\n\
    if (provider_counter() != 6) return 2;\n\
    if (egl_named != 14) return 3;\n\
    if (egl_plain != 10) return 4;\n\
    if (egl_fcounter != 103) return 5;\n\
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
            "omgc_extern_global_linkage_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("provider")).expect("create provider package");
        fs::create_dir_all(dir.join("consumer")).expect("create consumer package");
        fs::write(dir.join("provider/provider.omg"), PROVIDER).expect("write provider");
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

    /// The `_omg_` data symbols an object defines (`defined`) or leaves
    /// undefined.
    fn mangled_objects(&self, object: &str, defined: bool) -> Vec<String> {
        let table = String::from_utf8_lossy(&self.expect_ok("readelf", &["-sW", object]).stdout)
            .into_owned();
        let mut names: Vec<String> = table
            .lines()
            .filter(|line| line.contains(" UND ") != defined)
            .filter(|line| !defined || line.contains(" OBJECT "))
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
fn a_consumer_reads_and_writes_another_packages_globals() {
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
            "consumer-objects/consumer.o",
            "-o",
            "harness",
        ],
    );

    let run = workspace.run("./harness", &[]);
    assert!(
        run.status.success(),
        "the consumer must reach the provider's own storage; check {:?}",
        run.status.code()
    );

    if !tool_available("readelf") {
        eprintln!("skipping the symbol-table assertions: this needs 'readelf'");
        return;
    }
    let defined = workspace.mangled_objects("provider-objects/provider.o", true);
    let referenced = workspace.mangled_objects("consumer-objects/consumer.o", false);
    assert_eq!(
        defined.len(),
        1,
        "only `counter` keeps a mangled data symbol: {defined:?}"
    );
    assert_eq!(
        referenced, defined,
        "the consumer must reference the provider's mangled global symbol byte for byte"
    );
}
