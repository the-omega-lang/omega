//! The by-value size warning reads the shared layout owner, so the size it
//! reports is the type's real storage size: alignment padding counts, the
//! threshold is compared against that size, and pointer-width-dependent types
//! measure differently per target.

use omega_analyzer::error::AnalysisWarningKind;
use omega_analyzer::{Arch, Os, Target};
use omega_driver::{CompileError, CompiledProgram, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_large_struct_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn compile(&self, target: Target) -> CompiledProgram {
        match Driver::new(self.0.clone(), None, Vec::<ExternRoot>::new(), target)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())], target)
        {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got:\n{}", describe(&errors)),
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn describe(errors: &[CompileError]) -> String {
    errors
        .iter()
        .map(|error| match error {
            CompileError::Analysis { errors, .. } => errors
                .iter()
                .map(|error| error.kind.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `(type name, reported size)` of every by-value size warning.
fn reported_sizes(source: &str, target: Target) -> Vec<(String, u32)> {
    TestPackage::new(source)
        .compile(target)
        .warnings
        .iter()
        .filter_map(|(_, warning)| match &warning.kind {
            AnalysisWarningKind::LargeStructByValue { r#type, size } => {
                Some((r#type.to_string(), *size))
            }
            _ => None,
        })
        .collect()
}

fn riscv32() -> Target {
    Target {
        arch: Arch::Riscv32,
        os: Os::None,
    }
}

/// The whole point of reading shared layout: a type can be large *because* of
/// the padding an explicit alignment forces, while its fields sum to almost
/// nothing.
#[test]
fn alignment_padding_counts_toward_the_by_value_size() {
    const SOURCE: &str = r#"
        @layout(align = 64)
        struct Wide { exposed value: i64; }

        struct Holder {
            exposed lead: u8;
            exposed inner: Wide;
            exposed trail: u8;
        }

        @suppress(unused_parameter)
        takes(value: Holder) => void { }

        main() => void { }
    "#;

    // The three fields sum to 10 bytes; the real layout is 1 + 63 padding +
    // 8 + 56 padding + 1 + 63 trailing padding.
    assert_eq!(
        reported_sizes(SOURCE, Target::DEFAULT),
        vec![("Holder".to_string(), 192)]
    );
}

#[test]
fn the_threshold_is_compared_against_the_real_layout_size() {
    const AT_THRESHOLD: &str = r#"
        struct Exact { exposed data: [128]u8; }

        @suppress(unused_parameter)
        takes(value: Exact) => void { }

        main() => void { }
    "#;
    const OVER_THRESHOLD: &str = r#"
        struct OverByOne { exposed data: [129]u8; }

        @suppress(unused_parameter)
        takes(value: OverByOne) => void { }

        main() => void { }
    "#;

    assert!(
        reported_sizes(AT_THRESHOLD, Target::DEFAULT).is_empty(),
        "exactly the threshold is not over it"
    );
    assert_eq!(
        reported_sizes(OVER_THRESHOLD, Target::DEFAULT),
        vec![("OverByOne".to_string(), 129)]
    );
}

/// A pointer-width-dependent type is over the threshold on one target and
/// under it on another, so the decision cannot be made target-independently.
#[test]
fn the_reported_size_follows_the_target_pointer_width() {
    const SOURCE: &str = r#"
        struct Addresses { exposed data: [17]*u8; }

        @suppress(unused_parameter)
        takes(value: Addresses) => void { }

        main() => void { }
    "#;

    assert_eq!(
        reported_sizes(SOURCE, Target::DEFAULT),
        vec![("Addresses".to_string(), 136)]
    );
    assert!(
        reported_sizes(SOURCE, riscv32()).is_empty(),
        "17 four-byte pointers are 68 bytes and must not warn"
    );
}

/// A fat pointer is one pointer plus a 32-bit count, not a fixed 12 bytes.
#[test]
fn a_slice_field_measures_at_the_targets_own_width() {
    const SOURCE: &str = r#"
        struct Views { exposed data: [11]*[]u8; }

        @suppress(unused_parameter)
        takes(value: Views) => void { }

        main() => void { }
    "#;

    assert_eq!(
        reported_sizes(SOURCE, Target::DEFAULT),
        vec![("Views".to_string(), 132)]
    );
    assert!(
        reported_sizes(SOURCE, riscv32()).is_empty(),
        "11 eight-byte slices are 88 bytes and must not warn"
    );
}

#[test]
fn an_ordinary_small_aggregate_does_not_warn() {
    const SOURCE: &str = r#"
        struct Small { exposed a: i64; exposed b: i64; }

        @suppress(unused_parameter)
        takes(value: Small) => void { }

        main() => void { }
    "#;

    assert!(reported_sizes(SOURCE, Target::DEFAULT).is_empty());
    assert!(reported_sizes(SOURCE, riscv32()).is_empty());
}
