use crate::SourceModule;
use crate::diagnostics::ParseErrorKind;
use crate::parser::MAX_NESTING_DEPTH;

fn on_a_deep_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .expect("spawn test thread")
        .join()
        .expect("test thread panicked")
}

fn nested_parens(depth: usize) -> String {
    format!(
        "f() => i32 {{ x := {}1{}; x }}",
        "(".repeat(depth),
        ")".repeat(depth)
    )
}

fn nested_pointers(depth: usize) -> String {
    format!("f(p: {}i32) => void {{ }}", "*[?]".repeat(depth))
}

fn nesting_errors(source: &str) -> usize {
    match SourceModule::parse(source) {
        Ok(_) => 0,
        Err(errors) => errors
            .iter()
            .filter(|e| matches!(e.kind, ParseErrorKind::NestingTooDeep { .. }))
            .count(),
    }
}

#[test]
fn nesting_just_inside_the_limit_is_accepted() {
    // Two levels of slack: the function body's own block and the walrus
    // value each consume one before the parentheses start.
    let source = nested_parens(MAX_NESTING_DEPTH - 2);
    assert!(on_a_deep_stack(move || SourceModule::parse(&source).is_ok()));
}

#[test]
fn nesting_past_the_limit_is_a_diagnostic() {
    let source = nested_parens(MAX_NESTING_DEPTH + 8);
    assert_eq!(on_a_deep_stack(move || nesting_errors(&source)), 1);
}

#[test]
fn deeply_nested_types_are_bounded_too() {
    let source = nested_pointers(MAX_NESTING_DEPTH + 8);
    assert_eq!(on_a_deep_stack(move || nesting_errors(&source)), 1);
}

#[test]
fn the_nesting_limit_is_reported_once() {
    let deep = "(".repeat(MAX_NESTING_DEPTH + 8) + "1" + &")".repeat(MAX_NESTING_DEPTH + 8);
    let source = format!("f() => i32 {{ a := {deep}; b := {deep}; c := {deep}; 0 }}");
    assert_eq!(on_a_deep_stack(move || nesting_errors(&source)), 1);
}

#[test]
fn a_long_statement_sequence_is_not_nesting() {
    let body = "acc = acc + 1;\n".repeat(20_000);
    let source = format!("f() => i32 {{ mut acc := 0;\n{body}acc }}");
    assert!(SourceModule::parse(&source).is_ok());
}
