use omega_parser::SourceModule;
use omega_parser::prelude::Item;

fn function(source: &str) -> omega_parser::prelude::FunctionDefinitionStmt {
    let module = SourceModule::parse(source).unwrap();
    let Item::FunctionDefinition(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected function definition");
    };
    definition
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
fn spec_alias_plus_separator_parses() {
    let module = SourceModule::parse("spec X = A + B;").unwrap();
    let Item::Spec(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected spec definition");
    };
    assert!(definition.is_alias);
    assert_eq!(definition.dependencies.len(), 2);
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
