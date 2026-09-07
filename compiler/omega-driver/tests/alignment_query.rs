//! Driver-level behavior of `alignof<Type>`: it reports the Omega alignment
//! the shared layout owner computes, after generic substitution and through
//! aliases, and it is usable wherever a compile-time `usize` is. Also covers
//! the annotation rules that decide which alignments a target can accept.

use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::{Arch, Os, Target};
use omega_driver::{CompileError, Driver, ExternRoot};
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
            "omega_alignment_query_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn result(&self, target: Target) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::<ExternRoot>::new(), target)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())], target)
    }

    fn expect_ok(&self, target: Target) {
        if let Err(errors) = self.result(target) {
            panic!("expected this to compile, got:\n{}", describe(&errors));
        }
    }

    fn expect_errors(&self, target: Target) -> Vec<CompileError> {
        match self.result(target) {
            Ok(_) => panic!("expected this to be rejected, but it compiled"),
            Err(errors) => errors,
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
            CompileError::Parse { errors, .. } => errors
                .iter()
                .map(|error| format!("{:?}", error.kind))
                .collect::<Vec<_>>()
                .join("\n"),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn annotation_reasons(errors: &[CompileError]) -> Vec<String> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors
                .iter()
                .filter_map(|error| match &error.kind {
                    AnalysisErrorKind::InvalidAnnotationArgs { reason, .. } => Some(reason.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

/// A wrong `alignof` is a type error rather than a silent number: the query's
/// value becomes an array length whose type must match exactly.
const SHAPES: &str = r#"
@layout(align = 32)
struct Wide { exposed value: i64; }

struct Holder { exposed lead: u8; exposed inner: Wide; }

@layout(align = 1)
struct WeakOuter { exposed inner: Wide; }

union Bag { exposed byte: u8; exposed inner: Wide; }

enum Shape { Empty, Boxed { exposed body: Wide; }; }

alias Renamed = Wide;

struct Wrapper<T> { exposed item: T; }

@layout(align = sizeof<usize>)
struct PointerWide { exposed value: u8; }

comp POINTER_BYTES := sizeof<usize>;
"#;

fn assert_alignment(bindings: &str, uses: &str, target: Target) {
    TestPackage::new(&format!(
        r#"
        {SHAPES}
        {bindings}
        takes32(value: [32]u8) => void {{ }}
        takes1(value: [1]u8) => void {{ }}
        takes_pointer_wide(value: [POINTER_BYTES]u8) => void {{ }}
        entry_fn() => void {{
            {uses}
        }}
        "#
    ))
    .expect_ok(target);
}

#[test]
fn alignof_reports_the_effective_alignment_of_every_inline_shape() {
    assert_alignment(
        r#"
        comp A_WIDE := alignof<Wide>;
        comp A_HOLDER := alignof<Holder>;
        comp A_WEAK := alignof<WeakOuter>;
        comp A_BAG := alignof<Bag>;
        comp A_SHAPE := alignof<Shape>;
        comp A_ARRAY := alignof<[4]Wide>;
        comp A_ALIAS := alignof<Renamed>;
        comp A_GENERIC := alignof<Wrapper<Wide>>;
        "#,
        r#"
        mut wide: [A_WIDE]u8;
        mut holder: [A_HOLDER]u8;
        mut weak: [A_WEAK]u8;
        mut bag: [A_BAG]u8;
        mut shape: [A_SHAPE]u8;
        mut row: [A_ARRAY]u8;
        mut alias_of: [A_ALIAS]u8;
        mut generic: [A_GENERIC]u8;
        takes32(wide);
        takes32(holder);
        takes32(weak);
        takes32(bag);
        takes32(shape);
        takes32(row);
        takes32(alias_of);
        takes32(generic);
        "#,
        Target::DEFAULT,
    );
}

/// Omega primitives stay packed, and alignment never follows a pointer to
/// its pointee.
#[test]
fn alignof_reports_omega_alignment_not_natural_alignment() {
    assert_alignment(
        r#"
        comp A_U64 := alignof<u64>;
        comp A_F64 := alignof<f64>;
        comp A_USIZE := alignof<usize>;
        comp A_POINTER := alignof<*Wide>;
        comp A_SLICE := alignof<*[]Wide>;
        comp A_VOID := alignof<void>;
        comp A_FN := alignof<(a: i32) => i32>;
        "#,
        r#"
        mut a: [A_U64]u8;
        mut b: [A_F64]u8;
        mut c: [A_USIZE]u8;
        mut d: [A_POINTER]u8;
        mut e: [A_SLICE]u8;
        mut f: [A_VOID]u8;
        mut g: [A_FN]u8;
        takes1(a);
        takes1(b);
        takes1(c);
        takes1(d);
        takes1(e);
        takes1(f);
        takes1(g);
        "#,
        Target::DEFAULT,
    );
}

#[test]
fn a_pointer_width_dependent_annotation_follows_the_target() {
    for target in [
        Target::DEFAULT,
        Target {
            arch: Arch::Riscv32,
            os: Os::None,
        },
        Target {
            arch: Arch::Avr,
            os: Os::None,
        },
    ] {
        assert_alignment(
            "comp A_POINTER_WIDE := alignof<PointerWide>;",
            r#"
            mut value: [A_POINTER_WIDE]u8;
            takes_pointer_wide(value);
            "#,
            target,
        );
    }
}

/// The query is an ordinary contextual identifier, so a program may still use
/// `alignof` as a name where no `<Type>` follows.
#[test]
fn alignof_stays_a_contextual_identifier() {
    TestPackage::new(
        r#"
        alignof(value: i32) => i32 { value }
        entry_fn() => i32 { alignof(1) }
        "#,
    )
    .expect_ok(Target::DEFAULT);
}

#[test]
fn a_malformed_alignment_query_is_a_parse_error() {
    let errors = TestPackage::new(
        r#"
        entry_fn() => usize { alignof<> }
        "#,
    )
    .expect_errors(Target::DEFAULT);
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, CompileError::Parse { .. })),
        "an empty type argument must be a parse error, got:\n{}",
        describe(&errors)
    );
}

#[test]
fn a_spec_definition_is_not_a_value_type() {
    let errors = TestPackage::new(
        r#"
        exposed spec Greeter { greet(*self) => i32; }
        entry_fn() => usize { alignof<Greeter> }
        "#,
    )
    .expect_errors(Target::DEFAULT);
    assert!(
        !errors.is_empty(),
        "a spec definition has no value layout to report"
    );
}

#[test]
fn an_alignment_that_is_zero_or_not_a_power_of_two_is_rejected() {
    for (value, expected) in [("0", "power of two"), ("3", "power of two")] {
        let errors = TestPackage::new(&format!(
            r#"
            @layout(align = {value})
            struct Bad {{ exposed value: u8; }}
            entry_fn() => usize {{ sizeof<Bad> }}
            "#
        ))
        .expect_errors(Target::DEFAULT);
        let reasons = annotation_reasons(&errors);
        assert!(
            reasons.iter().any(|reason| reason.contains(expected)),
            "align = {value} must be rejected as not a power of two, got: {reasons:?}"
        );
    }
}

/// An alignment is an address requirement, so it must fit the target's
/// `usize`. A 16-bit target therefore accepts far less than the annotation
/// grammar's `u32`.
#[test]
fn an_alignment_the_target_cannot_represent_is_rejected() {
    const SOURCE: &str = r#"
        @layout(align = 65536)
        struct Huge { exposed value: u8; }
        entry_fn() => usize { sizeof<Huge> }
    "#;

    let avr = Target {
        arch: Arch::Avr,
        os: Os::None,
    };
    let reasons = annotation_reasons(&TestPackage::new(SOURCE).expect_errors(avr));
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("must fit this target's usize")),
        "a 16-bit target must reject an alignment it cannot hold, got: {reasons:?}"
    );

    // The same annotation is ordinary on a wider target.
    TestPackage::new(SOURCE).expect_ok(Target::DEFAULT);
}
