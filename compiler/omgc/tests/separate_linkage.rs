//! The linkage motivation for per-source objects: because each source file
//! owns its own object, a consumer that only needs one source can be linked
//! from an archive without dragging in another source's unmet dependency.
//!
//! This drives the real system toolchain (`ar`, `cc`); if either is missing
//! the case reports itself as skipped rather than failing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

const ANSWER: &str = "\
@symbol(mangle = disabled)\n\
exposed omega_answer() => i32 { 42 }\n";

/// This source depends on a capability nothing in the link provides. Its
/// object is therefore unlinkable on its own -- which is exactly the point:
/// a consumer of the other source must never be forced to resolve it.
const UNAVAILABLE: &str = "\
foreign(c) omega_absent_capability() => i32;\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_needs_absent() => i32 { omega_absent_capability() }\n";

const CONSUMER: &str = "\
int omega_answer(void);\n\
int main(void) { return omega_answer() == 42 ? 0 : 1; }\n";

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
            "omgc_separate_linkage_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("lib")).expect("create package root");
        fs::write(dir.join("lib/lib.omg"), ANSWER).expect("write library root");
        fs::write(dir.join("lib/unavailable.omg"), UNAVAILABLE).expect("write unlinkable source");
        fs::write(dir.join("consumer.c"), CONSUMER).expect("write consumer");
        Self(dir)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
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
fn an_archived_source_object_links_without_another_source_missing_capability() {
    if !tool_available("cc") || !tool_available("ar") {
        eprintln!("skipping: this case needs the system 'cc' and 'ar'");
        return;
    }

    let workspace = Workspace::new();
    workspace.expect_ok(env!("CARGO_BIN_EXE_omgc"), &["lib", "-o", "objects"]);
    assert!(
        Path::new(&workspace.path("objects/lib.o")).is_file()
            && Path::new(&workspace.path("objects/unavailable.o")).is_file(),
        "each source must own its object for archive selection to be possible"
    );

    // Passing every object directly still forces the missing capability: it is
    // whole-object selection, not per-source emission alone, that avoids it.
    let direct = workspace.run(
        "cc",
        &[
            "consumer.c",
            "objects/lib.o",
            "objects/unavailable.o",
            "-o",
            "direct",
        ],
    );
    assert!(
        !direct.status.success(),
        "linking the unavailable source's object must still require its dependency"
    );

    workspace.expect_ok(
        "ar",
        &[
            "rcs",
            "libomega.a",
            "objects/lib.o",
            "objects/unavailable.o",
        ],
    );
    workspace.expect_ok("cc", &["consumer.c", "libomega.a", "-o", "selected"]);

    let run = workspace.expect_ok("./selected", &[]);
    assert!(
        run.status.success(),
        "the selected source object must behave normally once linked"
    );
}
