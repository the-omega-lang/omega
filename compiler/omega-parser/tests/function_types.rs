use omega_parser::SourceModule;
use omega_parser::prelude::{AliasTarget, FunctionType, Item, ParseErrorKind, SelfMode, Type};

fn function_type(target: &str) -> FunctionType {
    let source = format!("alias F = {target};");
    let module = SourceModule::parse(&source).expect("function type must parse");
    match &module.nodes.last().expect("at least one item").item {
        Item::Alias(alias) => match &alias.target {
            AliasTarget::Type(Type::Function(f)) => f.clone(),
            other => panic!("expected a function type, found {other:?}"),
        },
        other => panic!("expected an alias item, found {other:?}"),
    }
}

/// Every parameter's descriptor, in written order.
fn descriptors(f: &FunctionType) -> Vec<Option<&str>> {
    f.params
        .iter()
        .map(|p| p.name.as_ref().map(|name| name.as_ref()))
        .collect()
}

#[test]
fn a_parameter_may_be_written_with_or_without_a_descriptor() {
    assert_eq!(descriptors(&function_type("(i32) => void")), [None]);
    assert_eq!(
        descriptors(&function_type("(name: i32) => void")),
        [Some("name")]
    );
    assert_eq!(
        descriptors(&function_type("(i32, ptr: *u8) => bool")),
        [None, Some("ptr")]
    );
}

#[test]
fn descriptors_are_not_part_of_the_written_type() {
    assert_eq!(
        function_type("(a: i32, *u8) => void"),
        function_type("(i32, b: *u8) => void")
    );
}

#[test]
fn a_pointer_typed_first_parameter_is_not_a_receiver() {
    for target in ["(*Thing) => void", "(*mut Thing) => void"] {
        let f = function_type(target);
        assert_eq!(f.self_mode, None);
        assert_eq!(descriptors(&f), [None]);
        assert!(matches!(f.params[0].r#type, Type::Pointer(_, _)));
    }
}

#[test]
fn receivers_still_parse_in_every_spelling() {
    for (target, mode) in [
        ("(self, i32) => void", SelfMode::Value),
        ("(mut self, i32) => void", SelfMode::MutValue),
        ("(*self, i32) => void", SelfMode::Pointer),
        ("(*mut self, i32) => void", SelfMode::MutPointer),
    ] {
        let f = function_type(target);
        assert_eq!(f.self_mode, Some(mode));
        assert_eq!(descriptors(&f), [None]);
    }
}

#[test]
fn nested_and_generic_parameter_types_parse_undescribed() {
    let f = function_type("(Pair<i32, *u8>, (i32) => i32, [4]u8) => void");
    assert_eq!(descriptors(&f), [None, None, None]);
    assert!(matches!(f.params[0].r#type, Type::Generic(_, _)));
    assert!(matches!(f.params[1].r#type, Type::Function(_)));
    assert!(matches!(f.params[2].r#type, Type::SizedArray(_, _)));
}

#[test]
fn a_foreign_variadic_tail_survives_undescribed_parameters() {
    let f = function_type("foreign(c) (*u8, count: usize, ...) => i32");
    assert!(f.is_variadic);
    assert_eq!(
        f.convention
            .as_ref()
            .expect("explicit convention")
            .name
            .as_ref(),
        "c"
    );
    assert_eq!(descriptors(&f), [None, Some("count")]);
}

#[test]
fn a_function_declaration_still_requires_named_parameters() {
    let errors = SourceModule::parse("f(i32) => void { }")
        .expect_err("a declaration parameter is a binding and must be named");

    assert!(errors.iter().any(|error| matches!(
        &error.kind,
        ParseErrorKind::Expected { expected, .. } if *expected == "':'"
    )));
}
