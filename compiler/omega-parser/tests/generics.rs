use omega_parser::SourceModule;
use omega_parser::prelude::{Item, Type};

fn function(source: &str) -> omega_parser::prelude::FunctionDefinitionStmt {
    let module = SourceModule::parse(source).unwrap();
    let Item::FunctionDefinition(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected function definition");
    };
    definition
}

fn param_type(source: &str) -> Type {
    function(source).params[0].r#type.clone()
}

#[test]
fn generic_bound_conjunction_parses_to_one_bound_per_member() {
    let function = function("f<T: A + B>(x: T) => void {}");
    assert_eq!(function.generics.len(), 1);
    assert_eq!(function.generics[0].bounds.len(), 2);
}

#[test]
fn generic_bound_conjunction_of_three_parses() {
    let function = function("f<T: A + B + C>(x: T) => void {}");
    assert_eq!(function.generics[0].bounds.len(), 3);
}

#[test]
fn unbounded_generic_has_no_bounds() {
    let function = function("f<T>(x: T) => void {}");
    assert!(function.generics[0].bounds.is_empty());
}

#[test]
fn bound_with_default_still_parses() {
    let function = function("f<T: A = i32>(x: T) => void {}");
    assert_eq!(function.generics[0].bounds.len(), 1);
    assert!(function.generics[0].default.is_some());
}

#[test]
fn bound_conjunction_with_default_parses() {
    let function = function("f<T: A + B = i32>(x: T) => void {}");
    assert_eq!(function.generics[0].bounds.len(), 2);
    assert!(function.generics[0].default.is_some());
}

#[test]
fn bound_conjunction_parses_on_structs_too() {
    let module = SourceModule::parse("struct S<T: A + B> { exposed x: T; }").unwrap();
    let Item::Struct(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected struct definition");
    };
    assert_eq!(definition.generics[0].bounds.len(), 2);
}

#[test]
fn spec_conjunction_type_parses_to_static_shape() {
    let ty = param_type("f(x: spec A + B) => void {}");
    let Type::SpecStatic(members) = ty else {
        panic!("expected a static spec conjunction, got {ty:?}");
    };
    assert_eq!(members.len(), 2);
}

#[test]
fn pointer_to_spec_conjunction_is_an_ordinary_structural_pointer() {
    let ty = param_type("f(x: *spec A + B) => void {}");
    let Type::Pointer(inner, mutable) = ty else {
        panic!("expected an ordinary pointer, got {ty:?}");
    };
    assert!(!mutable);
    assert!(matches!(*inner, Type::SpecStatic(members) if members.len() == 2));
}

#[test]
fn mut_pointer_to_spec_conjunction_is_an_ordinary_structural_pointer() {
    let ty = param_type("f(x: *mut spec A + B) => void {}");
    let Type::Pointer(inner, mutable) = ty else {
        panic!("expected an ordinary pointer, got {ty:?}");
    };
    assert!(mutable);
    assert!(matches!(*inner, Type::SpecStatic(members) if members.len() == 2));
}

#[test]
fn old_prefix_pointer_spec_object_syntax_is_rejected() {
    assert!(SourceModule::parse("f(x: spec *A) => void {}").is_err());
    assert!(SourceModule::parse("f(x: spec *mut A) => void {}").is_err());
}

#[test]
fn old_spec_alias_declaration_syntax_is_an_ordinary_parse_error() {
    // `spec Name = ...` is not recognized at all anymore -- no dedicated
    // diagnostic remembers that alias syntax used to exist; `=` where a
    // spec body is expected just fails like any other invalid syntax.
    let errors = SourceModule::parse("spec AB = A + B;").unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        &e.kind,
        omega_parser::diagnostics::ParseErrorKind::Expected { expected, .. }
            if *expected == "'{'"
    )));
}

#[test]
fn spec_provisioning_form_is_a_parse_error_naming_the_replacement() {
    let errors = SourceModule::parse("spec X : A, B { }").unwrap_err();
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        errors[0].kind,
        omega_parser::diagnostics::ParseErrorKind::SpecDependenciesRemoved
    ));
}

#[test]
fn spec_provisioning_form_with_single_dependency_is_rejected_too() {
    let errors = SourceModule::parse("spec X : A { }").unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        omega_parser::diagnostics::ParseErrorKind::SpecDependenciesRemoved
    )));
}
