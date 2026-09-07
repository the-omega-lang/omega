//! `std::alloc`'s aligned allocation must be a property of the adapter, not
//! of the host heap: this drives it over a raw `GlobalAllocator` whose blocks
//! are deliberately offset from every useful boundary, and which can be made
//! to fail on demand.
//!
//! It is a workflow case rather than a conformance one because a conformance
//! package cannot replace the platform's own allocator glue. If `cc` is
//! missing the case reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// Hands out blocks at `64k + 8`, so an adapter that returned the raw address
/// (or trusted the host heap's usual 16-byte alignment) would be caught.
const FIXTURE: &str = r#"
import std::alloc;

mut arena: [16384]u8;
mut used: usize = 0;
mut last_raw: usize = 0;
mut last_freed: usize = 0;
mut fail_next: bool = false;
mut raw_realloc_calls: usize = 0;

arena_base() => usize { <usize>&arena }

glue core::platform::GlobalAllocator {
    alloc(size: usize) => *mut u8 {
        if fail_next { return <*mut u8>0; }
        base := arena_base();
        mut start := base + used;
        start = start + (64 - start % 64) + 8;
        if start + size > base + 16384 { return <*mut u8>0; }
        used = start + size - base;
        last_raw = start;
        <*mut u8>start
    }

    free(ptr: *u8) => void {
        last_freed = <usize>ptr;
    }

    # `std::alloc` reallocates by allocate-copy-free, so it must never hand
    # an adjusted pointer back to the raw gap.
    realloc(ptr: *u8, size: usize) => *mut u8 {
        raw_realloc_calls = raw_realloc_calls + 1;
        last_freed = <usize>ptr;
        GlobalAllocator::alloc(size)
    }
}

@mangling(disabled)
exposed fixture_rounds_a_misaligned_raw_block() => i64 {
    block := alloc::alloc(32, 64);
    ok := <usize>block % 64 == 0 && last_raw % 64 == 8;
    alloc::free(block);
    if ok { 1i64 } else { 0i64 }
}

@mangling(disabled)
exposed fixture_free_recovers_the_raw_pointer() => i64 {
    block := alloc::alloc(32, 64);
    expected := last_raw;
    alloc::free(block);
    if last_freed == expected { 1i64 } else { 0i64 }
}

@mangling(disabled)
exposed fixture_reports_allocation_failure() => i64 {
    fail_next = true;
    block := alloc::alloc(32, 64);
    fail_next = false;
    if <usize>block == 0 { 1i64 } else { 0i64 }
}

@mangling(disabled)
exposed fixture_rejects_invalid_alignment() => i64 {
    zero := alloc::alloc(32, 0);
    odd := alloc::alloc(32, 24);
    if <usize>zero == 0 && <usize>odd == 0 { 1i64 } else { 0i64 }
}

@mangling(disabled)
exposed fixture_rejects_size_overflow() => i64 {
    huge := alloc::alloc(<usize>0 - 1usize, 64);
    if <usize>huge == 0 { 1i64 } else { 0i64 }
}

@mangling(disabled)
exposed fixture_never_reaches_the_raw_realloc() => i64 {
    block := alloc::alloc(8, 64);
    grown := alloc::realloc(block, 4096, 64);
    ok := <usize>grown != 0 && raw_realloc_calls == 0;
    alloc::free(grown);
    if ok { 1i64 } else { 0i64 }
}

@mangling(disabled)
exposed fixture_failed_realloc_keeps_the_old_block() => i64 {
    block := alloc::alloc(8, 64);
    *<*mut u8>(<usize>block) = 42u8;
    fail_next = true;
    replacement := alloc::realloc(block, 4096, 64);
    fail_next = false;
    if <usize>replacement == 0 && *<*u8>(<usize>block) == 42u8 { 1i64 } else { 0i64 }
}
"#;

const MAIN: &str = "\
long fixture_rounds_a_misaligned_raw_block(void);\n\
long fixture_free_recovers_the_raw_pointer(void);\n\
long fixture_reports_allocation_failure(void);\n\
long fixture_rejects_invalid_alignment(void);\n\
long fixture_rejects_size_overflow(void);\n\
long fixture_never_reaches_the_raw_realloc(void);\n\
long fixture_failed_realloc_keeps_the_old_block(void);\n\
\n\
int main(void) {\n\
    if (fixture_rounds_a_misaligned_raw_block() != 1) return 1;\n\
    if (fixture_free_recovers_the_raw_pointer() != 1) return 2;\n\
    if (fixture_reports_allocation_failure() != 1) return 3;\n\
    if (fixture_rejects_invalid_alignment() != 1) return 4;\n\
    if (fixture_rejects_size_overflow() != 1) return 5;\n\
    if (fixture_never_reaches_the_raw_realloc() != 1) return 6;\n\
    if (fixture_failed_realloc_keeps_the_old_block() != 1) return 7;\n\
    return 0;\n\
}\n";

fn runtime_root(package: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime")
        .join(package)
        .canonicalize()
        .unwrap_or_else(|error| panic!("runtime/{package} exists: {error}"))
}

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
            "omgc_aligned_glue_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir_all(dir.join("lib")).expect("create package root");
        fs::write(dir.join("lib/lib.omg"), "").expect("write package root module");
        fs::write(dir.join("lib/fixture.omg"), FIXTURE).expect("write fixture");
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
fn aligned_allocation_does_not_depend_on_the_raw_allocators_own_alignment() {
    if !tool_available("cc") {
        eprintln!("skipping: this case needs the system 'cc'");
        return;
    }

    let core_import = format!("--import=core:{}", runtime_root("core").display());
    let std_import = format!("--import=std:{}", runtime_root("std").display());
    let std_package = runtime_root("std");
    let std_package = std_package.to_string_lossy().into_owned();

    let workspace = Workspace::new();
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &[&std_package, "-o", "std", &core_import],
    );
    workspace.expect_ok(
        env!("CARGO_BIN_EXE_omgc"),
        &["lib", "-o", "objects", &core_import, &std_import],
    );
    workspace.expect_ok(
        "cc",
        &[
            "main.c",
            "objects/fixture.o",
            "objects/lib.o",
            "std/alloc.o",
            "-o",
            "program",
        ],
    );

    let run = workspace.run("./program", &[]);
    assert!(
        run.status.success(),
        "the aligned adapter failed check {:?} over a deliberately offset raw allocator",
        run.status.code()
    );
}
