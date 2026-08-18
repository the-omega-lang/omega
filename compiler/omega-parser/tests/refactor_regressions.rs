use omega_parser::SourceModule;
use omega_parser::diagnostics::ParseErrorKind;

#[test]
fn classic_for_uses_the_same_comp_binding_grammar_as_statements() {
    for binding in ["comp i := 0", "mut comp i := 0"] {
        let source = format!("main() => void {{ for {binding}; ; {{ }} }}");
        SourceModule::parse(&source)
            .expect("classic for initializers should accept statement binding modifiers");
    }
}

#[test]
fn annotation_sizeof_requires_an_opening_angle() {
    let errors = SourceModule::parse("@layout(pack = sizeof usize) struct S { }")
        .expect_err("sizeof without '<' must not parse as an annotation value");

    assert!(errors.iter().any(|error| matches!(
        &error.kind,
        ParseErrorKind::Expected { expected, .. }
            if *expected == "a plain integer, 'sizeof<Type>', or a string literal"
    )));
}

#[test]
fn dangling_annotation_reports_the_annotation_itself() {
    let errors = SourceModule::parse("@inline")
        .expect_err("a dangling annotation must be diagnosed explicitly");

    assert!(errors
        .iter()
        .any(|error| matches!(error.kind, ParseErrorKind::AnnotationWithoutItem)));
}

#[test]
fn duplicate_macro_parameters_are_rejected_at_the_definition() {
    let errors = SourceModule::parse("macro m($a: expr, $a: expr) => { $a }")
        .expect_err("duplicate macro parameters must be rejected");

    assert!(errors.iter().any(|error| matches!(
        &error.kind,
        ParseErrorKind::DuplicateMacroParam { name } if name.as_ref() == "a"
    )));
}

#[test]
fn variadic_macro_parameter_cannot_shadow_a_fixed_parameter() {
    let errors = SourceModule::parse("macro m($a: expr, $a: expr...) => { $...(){ $a } }")
        .expect_err("a variadic parameter must not duplicate a fixed parameter");

    assert!(errors.iter().any(|error| matches!(
        &error.kind,
        ParseErrorKind::DuplicateMacroParam { name } if name.as_ref() == "a"
    )));
}

#[test]
fn dangling_member_annotation_reports_the_annotation_itself() {
    let errors = SourceModule::parse("struct S { @inline }")
        .expect_err("a dangling member annotation must be diagnosed explicitly");

    assert!(errors
        .iter()
        .any(|error| matches!(error.kind, ParseErrorKind::AnnotationWithoutItem)));
}

#[test]
fn gap_and_glue_shape_errors_point_at_the_offending_member() {
    let gap_source = "gap G { f(self) => void; g() => void {} }";
    let gap_errors = SourceModule::parse(gap_source).expect_err("invalid gap members must fail");

    let self_error = gap_errors
        .iter()
        .find(|error| matches!(error.kind, ParseErrorKind::GapFunctionSelf { .. }))
        .expect("expected gap self error");
    assert_eq!(
        &gap_source[self_error.span.start..self_error.span.end],
        "f"
    );

    let body_error = gap_errors
        .iter()
        .find(|error| matches!(error.kind, ParseErrorKind::GapFunctionBody { .. }))
        .expect("expected gap body error");
    assert_eq!(
        &gap_source[body_error.span.start..body_error.span.end],
        "{}"
    );

    let glue_source = "glue G { f<T>() => void {} }";
    let glue_errors = SourceModule::parse(glue_source).expect_err("generic glue member must fail");
    let shape_error = glue_errors
        .iter()
        .find(|error| matches!(error.kind, ParseErrorKind::GlueFunctionShape { .. }))
        .expect("expected glue shape error");
    assert_eq!(
        &glue_source[shape_error.span.start..shape_error.span.end],
        "f"
    );
}
