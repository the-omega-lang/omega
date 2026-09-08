//! `@symbol(export)` is a binary-visibility decision, and a shared image is the
//! only place that decision is observable: an exported symbol reaches the
//! library's dynamic export table, a hidden one does not, and a consumer
//! resolves the exported ones across the image boundary at run time.
//!
//! This drives the real system toolchain (`cc`, `readelf`); if either is
//! missing the case reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// Compiled into a shared library. Only the two `export` items are meant to
/// leave the image; the other two are ordinary hidden definitions the library
/// still uses internally.
const LIBRARY: &str = "\
@symbol(export)\n\
exposed exported_value() => i32 { hidden_value() + 40 }\n\
\n\
exposed hidden_value() => i32 { 2 }\n\
\n\
@symbol(name = \"shared_exported_data\", export)\n\
exposed mut exported_data : i32 = 11;\n\
\n\
@symbol(name = \"shared_hidden_data\")\n\
exposed mut hidden_data : i32 = 13;\n\
\n\
@symbol(name = \"shared_read_hidden_data\", export)\n\
exposed read_hidden_data() => i32 { hidden_data }\n";

/// Reaches the library two ways: `shared::exported_value` through the extern
/// package, where the reference carries the definition's own `export`, and
/// `shared_exported_data` through a plain foreign import, which needs no
/// annotation because `foreign` already declares an external lookup.
const CONSUMER: &str = "\
import shared;\n\
\n\
foreign shared_exported_data : i32;\n\
\n\
@symbol(mangle = disabled)\n\
exposed consumer_call_exported() => i32 { shared::exported_value() }\n\
\n\
@symbol(mangle = disabled)\n\
exposed consumer_read_exported_data() => i32 { shared_exported_data }\n";

const HARNESS: &str = "\
int consumer_call_exported(void);\n\
int consumer_read_exported_data(void);\n\
\n\
int main(void) {\n\
    if (consumer_call_exported() != 42) return 1;\n\
    if (consumer_read_exported_data() != 11) return 2;\n\
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
            "omgc_shared_library_export_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("shared")).expect("create library package");
        fs::create_dir_all(dir.join("consumer")).expect("create consumer package");
        fs::write(dir.join("shared/shared.omg"), LIBRARY).expect("write library");
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
fn only_exported_symbols_leave_a_shared_library() {
    if !tool_available("cc") || !tool_available("readelf") {
        eprintln!("skipping: this case needs the system 'cc' and 'readelf'");
        return;
    }

    let workspace = Workspace::new();
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &["shared", "-o", "shared-objects"],
    );
    workspace.expect_ok(
        "cc",
        &["-shared", "shared-objects/shared.o", "-o", "libshared.so"],
    );

    let exports = String::from_utf8_lossy(
        &workspace
            .expect_ok("readelf", &["--dyn-syms", "-W", "libshared.so"])
            .stdout,
    )
    .into_owned();
    let exported: Vec<&str> = exports
        .lines()
        .filter_map(|line| line.split_whitespace().next_back())
        .collect();

    assert!(
        exported.contains(&"shared_exported_data")
            && exported.contains(&"shared_read_hidden_data")
            && exported
                .iter()
                .any(|symbol| symbol.contains("exported_value")),
        "every '@symbol(export)' item belongs to the library's dynamic exports:\n{exports}"
    );
    assert!(
        !exported.contains(&"shared_hidden_data")
            && !exported
                .iter()
                .any(|symbol| symbol.contains("hidden_value")),
        "a hidden definition must not reach the dynamic export table:\n{exports}"
    );

    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &[
            "consumer",
            "-o",
            "consumer-objects",
            "--import=shared:shared",
        ],
    );
    workspace.expect_ok(
        "cc",
        &[
            "harness.c",
            "consumer-objects/consumer.o",
            "-L.",
            "-lshared",
            "-Wl,-rpath,$ORIGIN",
            "-o",
            "program",
        ],
    );

    let run = workspace.run("./program", &[]);
    assert!(
        run.status.success(),
        "an exported function reached through an extern package and exported data \
         reached through a plain foreign import must both resolve across images; \
         check {:?}",
        run.status.code()
    );
}
