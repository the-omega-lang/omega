//! `core::range` -- ranges as tangible values.
//!
//! Every case here compiles against the *real* `runtime/core`, not a stub:
//! the feature is a claim about what `core` provides, so a synthesized
//! stand-in would prove nothing about the shipped library. Runtime semantics
//! (what a loop actually counts) live in `examples/range_demo` behind
//! `just test-range`, because they need a link and an execution; this file
//! covers what the front end accepts and rejects, and the diagnostics it
//! produces when it rejects.

use omega_analyzer::Target;
use omega_analyzer::error::AnalysisErrorKind;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::{Ident, ParseErrorKind};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_range_test_{}_{}",
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
            }], Target::DEFAULT)
        .expect("construct driver with the real core extern")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    fn expect_ok(&self) {
        if let Err(errors) = self.result() {
            panic!("expected this to compile, got: {errors:#?}");
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

/// The shipped `runtime/core`, located from this crate rather than the
/// process CWD so the tests do not depend on where cargo was invoked.
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

fn has_parse_error(errors: &[CompileError], kind: &ParseErrorKind) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Parse { errors, .. } => errors.iter().any(|error| &error.kind == kind),
        _ => false,
    })
}

// --- the headline feature -------------------------------------------------

/// The whole point: a range survives being bound to a name and iterated
/// later, through the ordinary `ToIterator` protocol.
#[test]
fn a_range_can_be_bound_to_a_name_and_iterated() {
    TestPackage::new(
        r#"
        main() => i32 {
            r := 1..<10;
            mut total := 0;
            for value in r { total = total + value; }
            total
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn ranges_are_inert_data_with_readable_fields() {
    TestPackage::new(
        r#"
        main() => i32 {
            r := 2..=8;
            if r.start == 2 { r.end } else { 0 }
        }
        "#,
    )
    .expect_ok();
}

/// A range is not its own cursor, so iterating one twice must be legal and
/// must not require `mut`. This is the property that makes the value/cursor
/// split worth having -- Rust's unified design cannot express it.
#[test]
fn the_same_range_value_can_be_iterated_twice() {
    TestPackage::new(
        r#"
        main() => i32 {
            r := 1..<4;
            mut first := 0;
            for value in r { first = first + 1; }
            mut second := 0;
            for value in r { second = second + 1; }
            first - second
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn a_range_passes_through_a_function_boundary() {
    TestPackage::new(
        r#"
        import extern::core::range::Range;
        widen(r: Range<i32>) => Range<i32> { r.start..<20 }
        main() => i32 {
            mut total := 0;
            for value in widen(1..<10) { total = total + value; }
            total
        }
        "#,
    )
    .expect_ok();
}

// --- the `{` ambiguity ----------------------------------------------------

/// Regression: `expression_starts_here` once counted `{` unconditionally, so
/// the loop body was consumed as the range's end bound and this failed with
/// `expected '{'`. The `for` header restricts struct literals; range parsing
/// has to honour that.
#[test]
fn an_open_ended_range_drives_a_for_loop() {
    TestPackage::new(
        r#"
        main() => i32 {
            mut n := 0;
            for value in 1.. {
                n = n + 1;
                if n == 3 { break; }
            }
            n
        }
        "#,
    )
    .expect_ok();
}

/// The same ambiguity from the other side: an *identifier* end bound sitting
/// immediately before the body's `{` must not be read as `stop { ... }`.
#[test]
fn a_named_end_bound_is_not_read_as_a_struct_literal() {
    TestPackage::new(
        r#"
        main() => i32 {
            start := 3;
            stop := 7;
            mut n := 0;
            for value in start..<stop { n = n + 1; }
            n
        }
        "#,
    )
    .expect_ok();
}

// --- `..` is the inference operator, never a bounded range ----------------

/// `..` means "infer this side" in every position. A range that writes both
/// bounds must say which kind it is, so `a..b` is a syntax error -- and,
/// critically, the *same* syntax error everywhere, rather than meaning one
/// thing in an expression and another in a slice.
#[test]
fn a_dotdot_range_with_an_end_is_rejected_in_expression_position() {
    let package = TestPackage::new("main() => i32 { r := 1..10; 0 }");
    assert!(has_parse_error(
        &package.expect_errors(),
        &ParseErrorKind::OpenRangeHasEnd
    ));
}

#[test]
fn a_dotdot_range_with_an_end_is_rejected_in_slice_position() {
    let package = TestPackage::new(
        "main() => i32 { arr : [4]i32 = [1,2,3,4]; s := &arr[1..3]; 0 }",
    );
    assert!(has_parse_error(
        &package.expect_errors(),
        &ParseErrorKind::OpenRangeHasEnd
    ));
}

#[test]
fn a_dotdot_range_with_an_end_is_rejected_in_pattern_position() {
    let package = TestPackage::new(
        "main() => i32 { x := 5; match x { 1..3 => { 1 } } else { 0 } }",
    );
    assert!(has_parse_error(
        &package.expect_errors(),
        &ParseErrorKind::OpenRangeHasEnd
    ));
}

// --- domain inference vs contextual inference -----------------------------

#[test]
fn an_open_end_infers_the_element_types_domain_maximum() {
    TestPackage::new(
        r#"
        main() => i32 {
            r := 1..;
            if r.end == 2147483647 { 0 } else { 1 }
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_open_start_infers_the_element_types_domain_minimum() {
    TestPackage::new(
        r#"
        main() => i32 {
            r := ..<10;
            if r.start == r.start { 0 } else { 1 }
        }
        "#,
    )
    .expect_ok();
}

/// Contextual inference wins in an index: `..` there means the container's
/// own length, never `usize`'s domain maximum.
#[test]
fn a_slice_still_infers_its_end_from_the_container() {
    TestPackage::new(
        r#"
        main() => i32 {
            arr : [4]i32 = [1,2,3,4];
            s := &arr[2..];
            <i32>s.length
        }
        "#,
    )
    .expect_ok();
}

/// ... and a base with no length has nothing to infer from, which is why
/// this stays an error rather than silently meaning `usize::MAX`.
#[test]
fn an_open_slice_end_needs_a_base_that_has_a_length() {
    let package = TestPackage::new(
        r#"
        rest(p: *[?]i32) => i32 {
            s := &p[2..];
            0
        }
        main() => i32 { 0 }
        "#,
    );
    assert!(has_analysis_error(&package.expect_errors(), |kind| matches!(
        kind,
        AnalysisErrorKind::MissingSliceEnd
    )));
}

/// Bare `..` infers *both* sides, so standalone it has no type source at all.
#[test]
fn a_bare_dotdot_is_rejected_with_no_context() {
    let package = TestPackage::new("main() => i32 { r := ..; 0 }");
    assert!(has_analysis_error(&package.expect_errors(), |kind| matches!(
        kind,
        AnalysisErrorKind::RangeNotAllowedHere
    )));
}

/// An expected type *is* context, so this one resolves.
#[test]
fn a_bare_dotdot_resolves_against_an_expected_range_type() {
    TestPackage::new(
        r#"
        import extern::core::range::Range;
        main() => i32 { r : Range<i32> = ..; if r.inclusive { 0 } else { 1 } }
        "#,
    )
    .expect_ok();
}

// --- user-extensibility ---------------------------------------------------

/// Requirement: ranges are not a primitive-only privilege. A user type that
/// conforms to `Successor` and `Bounded` is range-iterable on equal terms.
#[test]
fn a_user_type_conforming_to_successor_is_range_iterable() {
    TestPackage::new(
        r#"
        import extern::core::range::Successor;
        import extern::core::range::Bounded;
        import extern::core::cmp::Ord;
        import extern::core::cmp::Ordering;
        import extern::core::option::Option;

        struct PageIndex { exposed value: i32; }

        conform PageIndex to Ord {
            compare(*self, other: Self) => Ordering {
                if self.value < other.value { Ordering::Less }
                else if self.value > other.value { Ordering::Greater }
                else { Ordering::Equal }
            }
        }
        conform PageIndex to Successor {
            successor(*self) => Option<PageIndex> {
                if self.value == 2147483647 { return Option<PageIndex>::None; }
                Option<PageIndex>::Some { value = PageIndex { value = self.value + 1; }; }
            }
        }
        conform PageIndex to Bounded {
            min() => Self { PageIndex { value = 0; } }
            max() => Self { PageIndex { value = 2147483647; } }
        }

        main() => i32 {
            a := PageIndex { value = 2; };
            b := PageIndex { value = 5; };
            mut n := 0;
            for p in a..<b { n = n + 1; }
            n
        }
        "#,
    )
    .expect_ok();
}

/// An open bound needs a domain to infer from, and that is `Bounded`'s job.
/// The diagnostic must name `Bounded` rather than surfacing as a missing
/// `max` method on the element type.
#[test]
fn an_open_bound_without_bounded_names_the_missing_spec() {
    let package = TestPackage::new(
        r#"
        import extern::core::range::Successor;
        import extern::core::cmp::Ord;
        import extern::core::cmp::Ordering;
        import extern::core::option::Option;

        struct P { exposed v: i32; }
        conform P to Ord {
            compare(*self, other: Self) => Ordering {
                if self.v < other.v { Ordering::Less }
                else if self.v > other.v { Ordering::Greater }
                else { Ordering::Equal }
            }
        }
        conform P to Successor {
            successor(*self) => Option<P> { Option<P>::Some { value = P { v = self.v + 1; }; } }
        }

        main() => i32 { a := P { v = 1; }; r := a..; 0 }
        "#,
    );
    assert!(has_analysis_error(&package.expect_errors(), |kind| matches!(
        kind,
        AnalysisErrorKind::RangeNeedsBounded { .. }
    )));
}

// --- element types and their range protocol -------------------------------

/// `char` uses the ordinary `Successor` protocol. Its implementation skips
/// the UTF-16 surrogate hole rather than doing raw codepoint arithmetic.
#[test]
fn char_is_range_iterable() {
    TestPackage::new("main() => i32 { for c in 'a'..<'z' { } 0 }").expect_ok();
}

/// Floats have `Eq` but no total order and no successor, so they build a
/// `Range` value but cannot drive a loop.
#[test]
fn floats_are_not_range_iterable() {
    let package = TestPackage::new("main() => i32 { for f in 1.0..<2.0 { } 0 }");
    assert!(!package.expect_errors().is_empty());
}

// --- regression guard for the `cmp`/`numerics` reshuffle ------------------

/// `isize` once lost every inherent method here, because its conformances
/// were hand-expanded instead of going through `signed_integer$`, and that
/// macro also emits the `primitive` block. Nothing else in the tree calls
/// these, so only a direct test catches it.
#[test]
fn isize_keeps_its_inherent_primitive_methods() {
    TestPackage::new(
        r#"
        main() => i32 {
            x : isize = -5isize;
            y := x.abs();
            z := x.clamp(-1isize, 1isize);
            if x.is_negative() { <i32>(y + z) } else { 0 }
        }
        "#,
    )
    .expect_ok();
}

/// The same for `usize`, whose `Bounded` maximum is derived from an all-ones
/// bit pattern rather than a width-dependent literal.
#[test]
fn usize_ranges_iterate_and_keep_their_methods() {
    TestPackage::new(
        r#"
        main() => i32 {
            lo : usize = 1usize;
            hi : usize = 4usize;
            mut n := 0;
            for v in lo..<hi { n = n + 1; }
            if lo.is_odd() { n } else { 0 }
        }
        "#,
    )
    .expect_ok();
}

/// The overflow case the old `$more` flag existed for: an inclusive range
/// ending at the element type's own maximum must terminate without ever
/// computing `max + 1`.
#[test]
fn an_inclusive_range_reaching_the_domain_maximum_compiles() {
    TestPackage::new(
        r#"
        main() => i32 {
            lo : u8 = 253u8;
            mut n := 0;
            for v in lo..=255u8 { n = n + 1; }
            n
        }
        "#,
    )
    .expect_ok();
}

// --- `..` carries no end, but `..<`/`..=` are not `..` --------------------

/// The rejection above is about an end following bare `..`, not about
/// leading-open ranges in general. `..<b` and `..=b` are separate tokens and
/// stay valid everywhere -- over-tightening the rule would silently delete
/// the match arms and slices below.
#[test]
fn a_leading_open_range_is_valid_with_an_explicit_operator() {
    TestPackage::new(
        r#"
        main() => i32 {
            arr : [4]i32 = [1,2,3,4];
            s := &arr[..<2];
            x := 5;
            match x {
                ..=3 => { 1 },
                4..<9 => { <i32>s.length },
            } else { 0 }
        }
        "#,
    )
    .expect_ok();
}

/// `..5` is the same mistake as `a..b` seen from the other side: an end bound
/// after the token that means "no bound written here".
#[test]
fn an_end_may_not_follow_bare_dotdot_in_a_pattern() {
    let package = TestPackage::new("main() => i32 { x := 5; match x { ..5 => { 1 } } else { 0 } }");
    assert!(has_parse_error(
        &package.expect_errors(),
        &ParseErrorKind::OpenRangeHasEnd
    ));
}

#[test]
fn an_end_may_not_follow_bare_dotdot_in_a_slice() {
    let package =
        TestPackage::new("main() => i32 { arr : [4]i32 = [1,2,3,4]; s := &arr[..2]; 0 }");
    assert!(has_parse_error(
        &package.expect_errors(),
        &ParseErrorKind::OpenRangeHasEnd
    ));
}

// --- chars use the same range protocol, with a scalar-value successor -----

#[test]
fn char_ranges_compile_through_the_ordinary_successor_protocol() {
    TestPackage::new(
        r#"
        import extern::core::cmp::Ord;
        import extern::core::option::Option;
        needs_ord<T: Ord>(value: T) => T { value }
        main() => i32 {
            mut count := 0;
            for c in 'a'..='z' {
                needs_ord(c);
                count = count + 1;
            }
            count
        }
        "#,
    )
    .expect_ok();
}

// `char`'s own surface -- its constructor, classifiers and arithmetic rules --
// lives in `tests/char.rs`; the pointer-pair operator rule lives in
// `tests/pointer_arithmetic.rs`. Only range behaviour belongs here.
