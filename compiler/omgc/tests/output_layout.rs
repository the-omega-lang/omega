//! Black-box coverage of `omgc -o <dir>`: the output tree must mirror the
//! package's physical source tree exactly, one artifact per `.omg` file, with
//! the emit kind deciding only the extension.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// A temporary working directory holding a package root and whatever output
/// directories a case asks `omgc` to write.
struct Workspace(PathBuf);

impl Workspace {
    fn new(root_name: &str, files: &[(&str, &str)]) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "omgc_output_layout_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = dir.join(root_name);
        for (relative, source) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("source has a parent"))
                .expect("create source directory");
            fs::write(path, source).expect("write source");
        }
        Self(dir)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn compile(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_omgc"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .expect("run omgc")
    }

    fn expect_ok(&self, args: &[&str]) -> Output {
        let output = self.compile(args);
        assert!(
            output.status.success(),
            "omgc {args:?} failed:\n{}",
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

/// Every emitted file below `dir`, as `/`-separated relative paths in sorted
/// order.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = prefix.join(entry.file_name());
            if path.is_dir() {
                walk(&path, &relative, out);
            } else {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    let mut out = Vec::new();
    walk(dir, Path::new(""), &mut out);
    out.sort();
    out
}

const PACKAGE: [(&str, &str); 4] = [
    (
        "pkg.omg",
        "import self::helper;\nmain() => void { helper::used(); }\n",
    ),
    ("helper.omg", "exposed used() => void { }\n"),
    ("nested/nested.omg", "exposed deep() => i32 { 1 }\n"),
    ("silent.omg", "# no items\n"),
];

#[test]
fn the_output_tree_mirrors_the_source_tree_one_artifact_per_source() {
    let workspace = Workspace::new("pkg", &PACKAGE);
    // A path several levels below nothing that exists yet: `omgc` creates the
    // whole output tree, it does not require the caller to pre-make it.
    workspace.expect_ok(&["pkg", "-o", "build/objects"]);

    assert_eq!(
        tree(&workspace.path("build/objects")),
        vec![
            "helper.o".to_string(),
            "nested/nested.o".to_string(),
            "pkg.o".to_string(),
            "silent.o".to_string(),
        ]
    );
}

#[test]
fn each_emit_kind_only_changes_the_artifact_extension() {
    for (flag, extension) in [("--emit=ir", "ll"), ("--emit=asm", "s")] {
        let workspace = Workspace::new("pkg", &PACKAGE);
        workspace.expect_ok(&["pkg", "-o", "out", flag]);

        assert_eq!(
            tree(&workspace.path("out")),
            vec![
                format!("helper.{extension}"),
                format!("nested/nested.{extension}"),
                format!("pkg.{extension}"),
                format!("silent.{extension}"),
            ],
            "{flag} must keep the layout and change only the extension"
        );
    }
}

#[test]
fn a_windows_object_target_keeps_the_layout_and_takes_the_platform_extension() {
    let workspace = Workspace::new("pkg", &PACKAGE);
    workspace.expect_ok(&["pkg", "-o", "out", "--target=x86_64-windows"]);

    assert_eq!(
        tree(&workspace.path("out")),
        vec![
            "helper.obj".to_string(),
            "nested/nested.obj".to_string(),
            "pkg.obj".to_string(),
            "silent.obj".to_string(),
        ]
    );
}

/// A declared `<name>:<dir>` identity renames the module, and therefore the
/// symbols, but the artifacts stay where the sources are on disk.
#[test]
fn a_declared_package_identity_does_not_move_the_output_tree() {
    let workspace = Workspace::new("physical", &PACKAGE[1..]);
    fs::write(
        workspace.path("physical/physical.omg"),
        "import self::helper;\nmain() => void { helper::used(); }\n",
    )
    .expect("write root module");

    workspace.expect_ok(&["renamed:physical", "-o", "out"]);

    assert_eq!(
        tree(&workspace.path("out")),
        vec![
            "helper.o".to_string(),
            "nested/nested.o".to_string(),
            "physical.o".to_string(),
            "silent.o".to_string(),
        ],
        "the output mirrors disk layout, not the declared module identity"
    );
}

#[test]
fn an_existing_regular_file_is_rejected_as_an_output_directory() {
    let workspace = Workspace::new("pkg", &PACKAGE);
    fs::write(workspace.path("out.o"), b"stale object").expect("write blocking file");

    let output = workspace.compile(&["pkg", "-o", "out.o"]);
    assert!(!output.status.success(), "a regular file must be rejected");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a directory"), "{stderr}");
    assert_eq!(
        fs::read(workspace.path("out.o")).expect("blocking file still readable"),
        b"stale object",
        "a rejected output path must be left untouched"
    );
}

#[test]
fn the_help_and_usage_text_describe_an_output_directory() {
    let workspace = Workspace::new("pkg", &PACKAGE);

    let help = String::from_utf8_lossy(&workspace.expect_ok(&["--help"]).stdout).into_owned();
    assert!(help.contains("-o <output-dir>"), "{help}");
    assert!(help.contains("one artifact per source file"), "{help}");

    let missing_flag = workspace.compile(&["pkg"]);
    assert!(!missing_flag.status.success());
    let stderr = String::from_utf8_lossy(&missing_flag.stderr);
    assert!(stderr.contains("the -o <dir> flag is required"), "{stderr}");

    let no_arguments = workspace.compile(&[]);
    assert!(!no_arguments.status.success());
    let stderr = String::from_utf8_lossy(&no_arguments.stderr);
    assert!(stderr.contains("-o <output-dir>"), "{stderr}");
}
