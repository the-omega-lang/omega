//! The compiler-generated checks for illegal runtime control flow: which
//! operations get one, what an enum `else` is bounded to, and what the
//! generated call looks like once it reaches MIR.
//!
//! The executable behavior lives in the root conformance suite; what is
//! asserted here is the shape analysis hands downstream, which the suite can
//! only observe indirectly.

use omega_analyzer::Target;
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedBlock, CheckedExpr, CheckedExprNode, CheckedFunctionDef, CheckedItem, CheckedMatch,
    CheckedMatchRemainder, CheckedStmt,
};
use omega_analyzer::error::{AnalysisErrorKind, AnalysisWarningKind};
use omega_analyzer::runtime_checks::RuntimeCheck;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_mir::body::{MirBody, MirTerminator};
use omega_mir::mir::{MirFunctionBody, MirItem};
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
            "omega_runtime_checks_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn write_child(&self, name: &str, source: &str) {
        fs::write(self.0.join(format!("{name}.omg")), source).expect("write test child module");
    }

    fn compile_with(
        &self,
        externs: Vec<ExternRoot>,
    ) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, externs, Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())])
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        self.compile_with(vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core_root(),
        }])
    }

    fn expect_ok(&self) -> omega_driver::CompiledProgram {
        match self.compile() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
        }
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.compile() {
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

fn checked_function<'a>(
    program: &'a omega_driver::CompiledProgram,
    name: &str,
) -> &'a CheckedFunctionDef {
    program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .find_map(|item| match item {
            CheckedItem::FunctionDefinition(f) if f.name.as_ref() == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("'{name}' is checked"))
}

fn find_match(block: &CheckedBlock) -> Option<&CheckedMatch> {
    fn in_expr(expr: &CheckedExprNode) -> Option<&CheckedMatch> {
        match &expr.kind {
            CheckedExpr::Match(m) => Some(m),
            CheckedExpr::Codeblock(block) => find_match(block),
            _ => None,
        }
    }
    block
        .stmts
        .iter()
        .find_map(|stmt| match stmt {
            CheckedStmt::Expression(e) | CheckedStmt::Return(e) => in_expr(e),
            _ => None,
        })
        .or_else(|| block.tail.as_deref().and_then(in_expr))
}

fn mir_body(program: omega_driver::CompiledProgram, name: &str) -> MirBody {
    let entry = program.entry.clone();
    omega_mir::lower_program(program.modules, &entry)
        .into_iter()
        .flat_map(|(_, module)| module.items)
        .find_map(|item| match item {
            MirItem::FunctionDefinition(f) if f.name.as_ref() == name => match f.body {
                MirFunctionBody::Normal(body) => Some(body),
                MirFunctionBody::Naked(_) => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("'{name}' is lowered to an ordinary MIR body"))
}

/// A generated panic builds a `PanicInfo` whose message is a fixed
/// `MirExpr::String`, so the lowered body's own rendering says whether a
/// check was emitted -- and where -- without a second hand-written MIR walker
/// living in this test.
fn emits(body: &MirBody, check: RuntimeCheck) -> bool {
    format!("{body:?}").contains(check.message())
}

fn panic_block(body: &MirBody, check: RuntimeCheck) -> &omega_mir::body::MirBlockData {
    body.blocks
        .iter()
        .find(|block| format!("{:?}", block.statements).contains(check.message()))
        .unwrap_or_else(|| panic!("no block reports {check:?}"))
}

// -- What an enum `match` leaves for the check --------------------------

const SIGNAL: &str = "\
enum Signal {\n\
    Ready,\n\
    Busy,\n\
    Draining,\n\
}\n";

#[test]
fn an_enum_else_becomes_an_arm_over_the_variants_the_arms_left() {
    let package = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
             }} else {{\n\
                 2\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ));
    let program = package.expect_ok();
    let classify = checked_function(&program, "classify");
    let checked = find_match(&classify.body).expect("`classify` is a match");

    assert!(
        checked.else_branch.is_none(),
        "an enum `else` is not an unconditional fallthrough"
    );
    assert_eq!(
        checked.remainder,
        CheckedMatchRemainder::IllegalEnumTag,
        "what the arms leave is a tag outside the declared domain"
    );
    let conditions = &checked
        .arms
        .last()
        .expect("the `else` became the final arm")
        .conditions;
    assert_eq!(
        conditions.len(),
        2,
        "the `else` is bounded to exactly the two remaining variants, got {conditions:#?}"
    );
}

#[test]
fn a_value_match_keeps_its_ordinary_else_and_covered_remainder() {
    let package = TestPackage::new(
        "classify(n: i32) => i32 {\n\
             match n {\n\
                 0 => 1,\n\
             } else {\n\
                 2\n\
             }\n\
         }\n\
         main() => void { }",
    );
    let program = package.expect_ok();
    let checked =
        find_match(&checked_function(&program, "classify").body).expect("`classify` is a match");

    assert!(
        checked.else_branch.is_some(),
        "a value match's `else` stays an ordinary fallthrough"
    );
    assert_eq!(checked.remainder, CheckedMatchRemainder::Covered);
}

// -- A dead `else` -------------------------------------------------------

fn unreachable_else_span(
    program: &omega_driver::CompiledProgram,
) -> Option<omega_parser::prelude::Span> {
    program
        .warnings
        .iter()
        .find(|(_, warning)| matches!(warning.kind, AnalysisWarningKind::UnreachableCode))
        .map(|(_, warning)| warning.span)
}

#[test]
fn an_else_after_full_explicit_coverage_is_reported_unreachable() {
    let source = format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
                 Signal::Draining => 3,\n\
             }} else {{\n\
                 4\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    );
    let package = TestPackage::new(&source);
    let program = package.expect_ok();

    let span = unreachable_else_span(&program).expect("the dead `else` is reported");
    assert_eq!(
        &source[span.start..span.end.min(source.len())][..1],
        "{",
        "the finding is placed on the `else` block itself"
    );
    let checked =
        find_match(&checked_function(&program, "classify").body).expect("`classify` is a match");
    assert_eq!(
        checked.arms.len(),
        3,
        "the dead block is not emitted as a fourth arm"
    );
}

#[test]
fn an_else_after_a_catch_all_is_reported_unreachable() {
    let package = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 .. => 2,\n\
             }} else {{\n\
                 3\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ));
    let program = package.expect_ok();

    assert!(
        unreachable_else_span(&program).is_some(),
        "a `..` already covers the remainder, so the `else` after it is dead"
    );
    let checked =
        find_match(&checked_function(&program, "classify").body).expect("`classify` is a match");
    assert_eq!(checked.arms.len(), 2);
}

#[test]
fn a_dead_elses_result_type_does_not_join_the_match() {
    TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
                 Signal::Draining => 3,\n\
             }} else {{\n\
                 \"not an i32 at all\"\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ))
    .expect_ok();
}

#[test]
fn an_ordinary_error_inside_a_dead_else_is_still_reported() {
    let errors = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
                 Signal::Draining => 3,\n\
             }} else {{\n\
                 no_such_function()\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ))
    .expect_errors();

    assert!(
        !errors.is_empty(),
        "dead code is still source the programmer wrote"
    );
}

#[test]
fn missing_coverage_and_a_redundant_catch_all_keep_their_diagnostics() {
    let missing = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ))
    .expect_errors();
    assert!(
        has_analysis_error(&missing, |kind| matches!(
            kind,
            AnalysisErrorKind::NonExhaustiveMatchEnum { .. }
        )),
        "uncovered variants are still an error, not something the check absorbs: {missing:#?}"
    );

    let redundant = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
                 Signal::Draining => 3,\n\
                 .. => 4,\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ))
    .expect_errors();
    assert!(
        has_analysis_error(&redundant, |kind| matches!(
            kind,
            AnalysisErrorKind::CatchAllPatternRedundant
        )),
        "a `..` with no variants left is still an error: {redundant:#?}"
    );
}

// -- `?` resolves both tags ----------------------------------------------

#[test]
fn try_records_the_failure_tag_as_well_as_the_success_tag() {
    let program = TestPackage::new(
        "halve(value: Option<i32>) => Option<i32> {\n\
             inner := value?;\n\
             Option<i32>::Some { value = inner / 2; }\n\
         }\n\
         main() => void { }",
    )
    .expect_ok();

    fn find_try(block: &CheckedBlock) -> Option<&omega_analyzer::checked::CheckedTry> {
        block.stmts.iter().find_map(|stmt| match stmt {
            CheckedStmt::Declaration(_) => None,
            CheckedStmt::Expression(e) => match &e.kind {
                CheckedExpr::Assignment(a) => match &a.value.kind {
                    CheckedExpr::Try(r#try) => Some(r#try),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
    }

    let halve = checked_function(&program, "halve");
    let r#try = find_try(&halve.body).expect("`?` survives analysis as its own node");
    assert_ne!(
        r#try.source.success_tag, r#try.source.failure_tag,
        "both tags are resolved here, so MIR never reconstructs one of them"
    );
}

// -- Which calls are trusted ---------------------------------------------

#[test]
fn a_source_panic_is_not_instrumented_as_an_unexpected_return() {
    let program = TestPackage::new(
        "stop() => never {\n\
             panic$(\"stop\")\n\
         }\n\
         main() => void { }",
    )
    .expect_ok();
    let body = mir_body(program, "stop");

    assert!(
        !emits(&body, RuntimeCheck::NeverCallReturned),
        "the handler is the operation a check reports *through*, so guarding \
         its own call would make every generated panic instrument itself"
    );
    assert!(
        body.blocks
            .iter()
            .any(|block| matches!(block.terminator, MirTerminator::Unreachable)),
        "the continuation after the trusted call is a structural unreachable"
    );
}

#[test]
fn a_never_function_the_program_named_panic_is_still_guarded() {
    let program = TestPackage::new(
        "@naked\n\
         panic() => never {\n\
             asm() => {\n\
                 ret\n\
             }\n\
         }\n\
         caller() => void {\n\
             panic();\n\
         }\n\
         main() => void { }",
    )
    .expect_ok();

    assert!(
        emits(
            &mir_body(program, "caller"),
            RuntimeCheck::NeverCallReturned
        ),
        "the trusted endpoint is a resolved declaration, never a spelling"
    );
}

#[test]
fn a_dynamic_never_call_is_guarded() {
    let program = TestPackage::new(
        "exposed spec Terminator {\n\
             stop(*self) => never;\n\
         }\n\
         struct Sloppy { exposed marker: i32; }\n\
         meet Terminator for Sloppy {\n\
             @naked\n\
             stop(*self) => never {\n\
                 asm() => {\n\
                     ret\n\
                 }\n\
             }\n\
         }\n\
         caller(obj: *spec Terminator) => void {\n\
             obj.stop();\n\
         }\n\
         main() => void { }",
    )
    .expect_ok();

    assert!(
        emits(
            &mir_body(program, "caller"),
            RuntimeCheck::NeverCallReturned
        ),
        "a vtable slot has no statically known callee, so the guard is the only thing left"
    );
}

// -- What the generated check looks like in MIR --------------------------

#[test]
fn the_invalid_tag_block_reaches_the_handler_without_reading_a_payload() {
    let program = TestPackage::new(
        "halve(value: Option<i32>) => Option<i32> {\n\
             inner := value?;\n\
             Option<i32>::Some { value = inner / 2; }\n\
         }\n\
         main() => void { }",
    )
    .expect_ok();
    let body = mir_body(program, "halve");

    let block = panic_block(&body, RuntimeCheck::EnumTagInTry);
    assert!(
        matches!(block.terminator, MirTerminator::Unreachable),
        "the handler terminates, so its continuation is unreachable"
    );
    assert!(
        !format!("{:?}", block.statements).contains("EnumBody"),
        "neither payload is projected on the way to the handler"
    );
    assert_eq!(
        format!("{body:?}").matches("EnumTag").count(),
        1,
        "the dispatch tag is sampled once and branched on twice"
    );
}

#[test]
fn a_body_with_nothing_to_check_carries_no_panic_support() {
    let program =
        TestPackage::new("add(a: i32, b: i32) => i32 { a + b }\nmain() => void { }").expect_ok();

    assert!(
        checked_function(&program, "add").runtime_checks.is_none(),
        "a body that reaches no checked operation must not gain a reference to the panic gap"
    );
}

#[test]
fn the_generated_call_reaches_the_gap_through_the_ordinary_glued_catalog() {
    let program = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
                 Signal::Draining => 3,\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ))
    .expect_ok();

    let handler = program
        .extern_functions
        .iter()
        .find(|f| {
            matches!(&f.kind, omega_analyzer::checked::ExternFunctionKind::Free(name)
                if name.as_ref() == "panic")
        })
        .expect("the panic gap is catalogued like any other extern reference");
    assert!(
        matches!(
            &handler.symbol.mangling,
            ManglingMode::Glued { spec_name, .. } if spec_name.as_ref() == "PanicHandler"
        ),
        "no mangled name is hard-coded: the gap's own glued policy names it"
    );
    assert_eq!(
        *handler.fn_type.return_type,
        omega_analyzer::resolved_type::ResolvedType::Never,
        "the catalogued signature is the gap's own"
    );
}

#[test]
fn a_generic_bodys_check_reports_the_file_the_template_was_written_in() {
    let package = TestPackage::new(
        "import self::helper;\n\
         main() => void {\n\
             helper::classify<i32>(helper::Signal::Ready);\n\
         }",
    );
    package.write_child(
        "helper",
        "exposed enum Signal {\n\
             Ready,\n\
             Busy,\n\
         }\n\
         exposed classify<T>(signal: Signal) => i32 {\n\
             match signal {\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
             }\n\
         }",
    );
    let program = package.expect_ok();
    let body = mir_body(program, "classify");

    assert!(
        emits(&body, RuntimeCheck::EnumTagInMatch),
        "an instantiated generic body is checked like any other"
    );
    assert!(
        format!("{body:?}").contains("helper.omg"),
        "the instantiation keeps the defining source, not the module it was emitted for"
    );
}

#[test]
fn a_guarded_call_keeps_what_ran_before_it_and_drops_what_would_have_run_after() {
    let program = TestPackage::new(
        "@naked
         returns_anyway(code: i32) => never {
             asm() => {
                 ret
             }
         }
         side_effect() => i32 { 7 }
         after() => void { }
         caller() => void {
             returns_anyway(side_effect());
             after();
         }
         main() => void { }",
    )
    .expect_ok();
    let body = mir_body(program, "caller");

    assert!(
        emits(&body, RuntimeCheck::NeverCallReturned),
        "the call site is guarded"
    );
    // `side_effect()`, `returns_anyway(...)` and the handler -- and nothing
    // else. A fourth would be either `after()`, which this call was supposed
    // to precede, or the argument evaluated a second time.
    assert_eq!(
        format!("{body:?}").matches("MirFunctionCall").count(),
        3,
        "exactly the argument, the guarded call, and the handler are emitted"
    );
    let entry = format!("{:?}", body.blocks[0].statements);
    assert!(
        entry.find("MirFunctionCall").unwrap()
            < entry
                .find(RuntimeCheck::NeverCallReturned.message())
                .unwrap(),
        "the argument's own evaluation still happens before the call that reports the broken contract"
    );
}

#[test]
fn a_never_call_in_tail_position_is_guarded() {
    let program = TestPackage::new(
        "@naked
         returns_anyway() => never {
             asm() => {
                 ret
             }
         }
         caller() => never {
             returns_anyway()
         }
         main() => void { }",
    )
    .expect_ok();

    assert!(emits(
        &mir_body(program, "caller"),
        RuntimeCheck::NeverCallReturned
    ));
}

#[test]
fn a_structurally_unreachable_block_is_not_instrumented() {
    let program = TestPackage::new(
        "spin() => never {
             loop { }
         }
         main() => void { }",
    )
    .expect_ok();
    let body = mir_body(program, "spin");

    assert!(
        body.blocks
            .iter()
            .any(|block| matches!(block.terminator, MirTerminator::Unreachable)),
        "a loop with no reachable break still ends in a structural unreachable"
    );
    assert!(
        !emits(&body, RuntimeCheck::NeverCallReturned)
            && !emits(&body, RuntimeCheck::EnumTagInMatch),
        "a block with no predecessor is not a runtime invariant anything can violate"
    );
}

// -- Missing or incompatible core support ---------------------------------

#[test]
fn a_check_without_core_is_a_source_located_diagnostic() {
    let package = TestPackage::new(&format!(
        "{SIGNAL}\
         classify(signal: Signal) => i32 {{\n\
             match signal {{\n\
                 Signal::Ready => 1,\n\
                 Signal::Busy => 2,\n\
                 Signal::Draining => 3,\n\
             }}\n\
         }}\n\
         main() => void {{ }}"
    ));
    let errors = match package.compile_with(Vec::new()) {
        Ok(_) => panic!("a check with no panic contract to call must be reported"),
        Err(errors) => errors,
    };

    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::RuntimeCheckSupportUnavailable { .. }
        )),
        "expected a normal diagnostic rather than a compiler panic: {errors:#?}"
    );
    let located = errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| {
            matches!(
                error.kind,
                AnalysisErrorKind::RuntimeCheckSupportUnavailable { .. }
            ) && error.span.end > error.span.start
        }),
        _ => false,
    });
    assert!(located, "the diagnostic names a real source span");
}

#[test]
fn a_body_with_no_check_still_compiles_without_core() {
    TestPackage::new("add(a: i32, b: i32) => i32 { a + b }\nmain() => void { }")
        .compile_with(Vec::new())
        .expect("nothing here needs `core`'s panic contract");
}

#[test]
fn a_nested_never_argument_keeps_earlier_sibling_calls() {
    let program = TestPackage::new(
        "foreign(c) stop() => never;
         before() => i32 { 7 }
         after() => i32 { 9 }
         consume(a: i32, b: i32, c: i32) => void { }
         caller(flag: bool) => void { consume(before(), if flag { stop() } else { 0 }, after()); }
         main() => void { }",
    )
    .expect_ok();
    let body = mir_body(program, "caller");
    assert_eq!(
        format!("{:?}", body.blocks[0].statements)
            .matches("MirFunctionCall")
            .count(),
        1,
        "before() must execute before branching into the possibly diverging second argument"
    );
}

#[test]
fn a_dead_else_does_not_inherit_the_match_literal_type() {
    TestPackage::new(&format!(
        "{SIGNAL}
         classify(signal: Signal) => u8 {{
             match signal {{
                 Signal::Ready => 1,
                 Signal::Busy => 2,
                 Signal::Draining => 3,
             }} else {{ 1000 }}
         }}
         main() => void {{ }}"
    ))
    .expect_ok();
}

#[test]
fn aggregate_and_binary_operands_keep_effects_before_a_later_panic() {
    for expression in [
        "before() + if flag { stop() } else { 0 }",
        "[before(), if flag { stop() } else { 0 }]",
        "Pair { a = before(); b = if flag { stop() } else { 0 }; }",
    ] {
        let program = TestPackage::new(&format!(
            "foreign(c) stop() => never;
             before() => i32 {{ 7 }}
             struct Pair {{ exposed a: i32; exposed b: i32; }}
             caller(flag: bool) => void {{ value := {expression}; }}
             main() => void {{ }}"
        ))
        .expect_ok();
        let body = mir_body(program, "caller");
        assert_eq!(
            format!("{:?}", body.blocks[0].statements)
                .matches("MirFunctionCall")
                .count(),
            1,
            "the first operand must be evaluated before the branch in {expression}"
        );
    }
}

#[test]
fn never_calls_are_guarded_in_expected_value_positions() {
    for statement in [
        "consume(stop());",
        "consume(if flag { stop() } else { stop() });",
        "value: Pair = Pair { a = before(); b = stop(); };",
        "value: [2]i32 = [before(), stop()];",
        "value: i32 = stop();",
        "mut value: i32 = 0; value = stop();",
        "return stop();",
        "value := flag && stop();",
        "value := flag || stop();",
        "value := stop() && flag;",
        "value := stop() || flag;",
        "generic<i32>(stop());",
        "indirect: (i32) => void = consume; indirect(stop());",
    ] {
        let program = TestPackage::new(&format!(
            "foreign(c) stop() => never;
             before() => i32 {{ 7 }}
             consume(value: i32) => void {{ }}
             generic<T>(value: T) => void {{ }}
             struct Pair {{ exposed a: i32; exposed b: i32; }}
             caller(flag: bool) => void {{ {statement} }}
             main() => void {{ }}"
        ))
        .expect_ok();
        assert!(
            emits(
                &mir_body(program, "caller"),
                RuntimeCheck::NeverCallReturned
            ),
            "{statement}"
        );
    }
}
