//! Driver-level behavior of compile-time value generics: instantiation
//! identity, substitution, inference, defaults, and the rules that reject a
//! value that is not compile-time or not representable.

use omega_analyzer::Target;
use omega_analyzer::error::{AnalysisErrorKind, TypeResolutionError};
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
            "omega_comp_generics_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn result(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(
            self.0.clone(),
            None,
            vec![ExternRoot {
                name: Ident("core".to_string()),
                dir: core_root(),
            }],
            Target::DEFAULT,
        )
        .expect("construct driver with the real core extern")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    /// Reports failures through each finding's own message: a resolved type
    /// graph contains reference cycles, so `Debug`-formatting the raw errors
    /// would not terminate.
    fn expect_ok(&self) {
        if let Err(errors) = self.result() {
            panic!("expected this to compile, got:\n{}", describe(&errors));
        }
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.result() {
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
            CompileError::Resolve { error, .. } => error.to_string(),
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

fn core_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/core")
        .canonicalize()
        .expect("runtime/core exists")
}

fn has_analysis_error(
    errors: &[CompileError],
    predicate: impl Fn(&AnalysisErrorKind) -> bool,
) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| predicate(&error.kind)),
        _ => false,
    })
}

fn has_type_resolution_error(
    errors: &[CompileError],
    predicate: impl Fn(&TypeResolutionError) -> bool + Copy,
) -> bool {
    has_analysis_error(errors, |kind| match kind {
        AnalysisErrorKind::UnresolvedType(error) => predicate(error),
        _ => false,
    })
}

const BUFFER: &str = r#"
struct Buffer<comp N: usize, T> {
    exposed data: [N]T;

    exposed len(*self) => usize { N }
}
"#;

#[test]
fn a_comp_generic_is_usable_as_a_compile_time_value_inside_the_body() {
    TestPackage::new(&format!(
        r#"
        {BUFFER}
        entry_fn() => i32 {{
            b := Buffer<2, i32> {{ data = [1, 2]; }};
            <i32>b.len()
        }}
        "#
    ))
    .expect_ok();
}

#[test]
fn equal_instantiations_from_different_spellings_share_one_type() {
    // The declared `usize` parameter is authoritative, so an `i32`-typed
    // `comp` binding holding 2 must reach the same instantiation a literal
    // `2` does -- otherwise these two bindings would have unequal types.
    TestPackage::new(&format!(
        r#"
        {BUFFER}
        comp size := 2;
        entry_fn() => i32 {{
            mut a := Buffer<2, i32> {{ data = [1, 2]; }};
            b := Buffer<size, i32> {{ data = [3, 4]; }};
            a = b;
            a.data[0]
        }}
        "#
    ))
    .expect_ok();
}

#[test]
fn differing_comp_values_are_differing_types() {
    let errors = TestPackage::new(&format!(
        r#"
        {BUFFER}
        entry_fn() => i32 {{
            mut a := Buffer<2, i32> {{ data = [1, 2]; }};
            b := Buffer<3, i32> {{ data = [3, 4, 5]; }};
            a = b;
            0
        }}
        "#
    ))
    .expect_errors();
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::AssignmentTypeMismatch { .. }
    )));
}

#[test]
fn a_comp_generic_binds_from_a_concrete_fixed_array_argument() {
    TestPackage::new(
        r#"
        count<comp N: usize, T>(values: [N]T) => usize { N }
        entry_fn() => i32 {
            values: [3]i32 = [1, 2, 3];
            <i32>count(values)
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn a_runtime_binding_is_never_a_comp_generic_argument() {
    let errors = TestPackage::new(&format!(
        r#"
        {BUFFER}
        entry_fn() => i32 {{
            not_comp := 2;
            b := Buffer<not_comp, i32> {{ data = [1, 2]; }};
            0
        }}
        "#
    ))
    .expect_errors();
    assert!(has_type_resolution_error(&errors, |error| matches!(
        error,
        TypeResolutionError::NotACompValue(name) if name.as_ref() == "not_comp"
    )));
}

#[test]
fn a_type_in_a_comp_slot_is_reported_as_a_kind_mismatch() {
    let errors = TestPackage::new(&format!(
        r#"
        {BUFFER}
        entry_fn() => i32 {{
            b := Buffer<i32, i32> {{ data = [1]; }};
            0
        }}
        "#
    ))
    .expect_errors();
    assert!(has_type_resolution_error(&errors, |error| matches!(
        error,
        TypeResolutionError::CompValueIsAType(name) if name.as_ref() == "i32"
    )));
}

#[test]
fn a_value_in_a_type_slot_is_reported_as_a_kind_mismatch() {
    let errors = TestPackage::new(
        r#"
        struct Pair<A, B> { exposed a: A; exposed b: B; }
        entry_fn() => i32 {
            p := Pair<1, i32> { a = 1; b = 2; };
            0
        }
        "#,
    )
    .expect_errors();
    assert!(has_type_resolution_error(&errors, |error| matches!(
        error,
        TypeResolutionError::GenericArgKindMismatch {
            expected_value: false,
            ..
        }
    )));
}

#[test]
fn an_unsupported_comp_parameter_type_is_rejected_at_the_use_site() {
    let errors = TestPackage::new(
        r#"
        struct Weighted<comp W: f64> { exposed value: i32; }
        entry_fn() => i32 {
            w := Weighted<1> { value = 0; };
            0
        }
        "#,
    )
    .expect_errors();
    assert!(has_type_resolution_error(&errors, |error| matches!(
        error,
        TypeResolutionError::UnsupportedCompParamType { .. }
    )));
}

#[test]
fn a_value_that_does_not_fit_the_declared_type_is_rejected() {
    let errors = TestPackage::new(
        r#"
        struct Small<comp N: u8> { exposed data: [N]u8; }
        entry_fn() => i32 {
            s := Small<300> { data = [1u8]; };
            0
        }
        "#,
    )
    .expect_errors();
    assert!(has_type_resolution_error(&errors, |error| matches!(
        error,
        TypeResolutionError::CompArgNotRepresentable { value, .. } if value == "300"
    )));
}

#[test]
fn a_dependent_array_length_outside_the_size_domain_is_rejected() {
    let errors = TestPackage::new(&format!(
        r#"
        {BUFFER}
        entry_fn() => i32 {{
            b := Buffer<5000000000, u8> {{ data = [1u8]; }};
            0
        }}
        "#
    ))
    .expect_errors();
    assert!(has_type_resolution_error(&errors, |error| matches!(
        error,
        TypeResolutionError::InvalidArrayLength { .. }
    )));
}

#[test]
fn a_comp_default_fills_an_omitted_trailing_argument() {
    TestPackage::new(
        r#"
        struct Block<comp N: usize = 4, comp M: usize = N> {
            exposed rows: [N]u8;
            exposed cols: [M]u8;
        }
        entry_fn() => i32 {
            mut a := Block { rows = [1u8, 1u8, 1u8, 1u8]; cols = [2u8, 2u8, 2u8, 2u8]; };
            b: Block<4, 4> = Block { rows = [3u8, 3u8, 3u8, 3u8]; cols = [4u8, 4u8, 4u8, 4u8]; };
            a = b;
            <i32>a.rows[0]
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_alias_forwards_comp_parameters_without_changing_identity() {
    TestPackage::new(&format!(
        r#"
        {BUFFER}
        alias Two<T> = Buffer<2, T>;
        alias Same<comp N: usize, T> = Buffer<N, T>;
        entry_fn() => i32 {{
            mut a: Two<i32> = Buffer<2, i32> {{ data = [1, 2]; }};
            b: Same<2, i32> = Buffer<2, i32> {{ data = [3, 4]; }};
            a = b;
            a.data[1]
        }}
        "#
    ))
    .expect_ok();
}

#[test]
fn an_unresolvable_comp_parameter_reports_inference_failure() {
    let errors = TestPackage::new(
        r#"
        undetermined<comp N: usize, T>(value: T) => T { value }
        entry_fn() => i32 { undetermined(1) }
        "#,
    )
    .expect_errors();
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::UnresolvedGenericParam(name) if name.as_ref() == "N"
    )));
}

#[test]
fn a_conformance_can_target_a_comp_generic_instantiation() {
    TestPackage::new(&format!(
        r#"
        {BUFFER}
        spec Sized {{
            size(*self) => usize;
        }}
        meet Sized for Buffer<2, i32> {{
            size(*self) => usize {{ self.len() }}
        }}
        entry_fn() => i32 {{
            b := Buffer<2, i32> {{ data = [1, 2]; }};
            <i32><Buffer<2, i32> : Sized>::size(b)
        }}
        "#
    ))
    .expect_ok();
}

#[test]
fn a_generic_conform_template_binds_a_comp_parameter() {
    TestPackage::new(&format!(
        r#"
        {BUFFER}
        spec Sized {{
            size(*self) => usize;
        }}
        meet<comp N: usize, T> Sized for Buffer<N, T> {{
            size(*self) => usize {{ N }}
        }}
        entry_fn() => i32 {{
            two := Buffer<2, i32> {{ data = [1, 2]; }};
            three := Buffer<3, u8> {{ data = [1u8, 2u8, 3u8]; }};
            <i32>(<Buffer<2, i32> : Sized>::size(two) + <Buffer<3, u8> : Sized>::size(three))
        }}
        "#
    ))
    .expect_ok();
}
