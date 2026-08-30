use omega_parser::SourceModule;
use omega_parser::prelude::{ArrayLength, CompLiteral, GenericArg, Item, Type};

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
    assert_eq!(function.generics[0].bounds().len(), 2);
}

#[test]
fn generic_bound_conjunction_of_three_parses() {
    let function = function("f<T: A + B + C>(x: T) => void {}");
    assert_eq!(function.generics[0].bounds().len(), 3);
}

#[test]
fn unbounded_generic_has_no_bounds() {
    let function = function("f<T>(x: T) => void {}");
    assert!(function.generics[0].bounds().is_empty());
}

#[test]
fn bound_with_default_still_parses() {
    let function = function("f<T: A = i32>(x: T) => void {}");
    assert_eq!(function.generics[0].bounds().len(), 1);
    assert!(function.generics[0].default.is_some());
}

#[test]
fn bound_conjunction_with_default_parses() {
    let function = function("f<T: A + B = i32>(x: T) => void {}");
    assert_eq!(function.generics[0].bounds().len(), 2);
    assert!(function.generics[0].default.is_some());
}

#[test]
fn bound_conjunction_parses_on_structs_too() {
    let module = SourceModule::parse("struct S<T: A + B> { exposed x: T; }").unwrap();
    let Item::Struct(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected struct definition");
    };
    assert_eq!(definition.generics[0].bounds().len(), 2);
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

// --- `comp` generic parameters and mixed generic arguments ---------------

fn struct_def(source: &str) -> omega_parser::prelude::StructStmt {
    let module = SourceModule::parse(source).unwrap();
    let Item::Struct(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected struct definition");
    };
    definition
}

fn generic_args_of(ty: &Type) -> &[GenericArg] {
    match ty {
        Type::Generic(_, args) => args,
        other => panic!("expected a generic type, found {other:?}"),
    }
}

#[test]
fn comp_generic_param_records_its_value_type() {
    let function = function("f<comp N: usize>() => void {}");
    assert!(function.generics[0].is_comp());
    assert!(function.generics[0].bounds().is_empty());
    assert!(matches!(
        function.generics[0].comp_type(),
        Some(Type::Named(path)) if path.head.as_ref() == "usize"
    ));
}

#[test]
fn comp_generic_param_requires_a_value_type() {
    assert!(SourceModule::parse("f<comp N>() => void {}").is_err());
}

#[test]
fn comp_is_still_usable_as_a_type_parameter_name() {
    let function = function("f<comp>(x: comp) => void {}");
    assert_eq!(function.generics.len(), 1);
    assert!(!function.generics[0].is_comp());
    assert_eq!(function.generics[0].ident.as_ref(), "comp");
}

#[test]
fn comp_generic_param_accepts_a_value_default() {
    let function = function("f<comp N: usize = 16>() => void {}");
    assert!(matches!(
        function.generics[0].default,
        Some(GenericArg::Value(CompLiteral::Int {
            negative: false,
            ..
        }))
    ));
}

#[test]
fn comp_generic_default_may_name_an_earlier_parameter() {
    let function = function("f<comp N: usize = 16, comp M: usize = N>() => void {}");
    assert!(matches!(
        &function.generics[1].default,
        Some(GenericArg::Type(Type::Named(path))) if path.head.as_ref() == "N"
    ));
}

#[test]
fn mixed_generic_arguments_in_a_type_path_keep_their_written_kind() {
    let ty = param_type("f(x: Buffer<10, i32>) => void {}");
    let args = generic_args_of(&ty);
    assert!(matches!(
        args[0],
        GenericArg::Value(CompLiteral::Int {
            negative: false,
            ..
        })
    ));
    assert!(matches!(&args[1], GenericArg::Type(Type::Named(_))));
}

#[test]
fn a_bare_path_generic_argument_stays_type_syntax() {
    // `SIZE` is only decided by the declared parameter's kind, so the parser
    // must not guess: it stays a written type path either way.
    let ty = param_type("f(x: Buffer<SIZE, T>) => void {}");
    let args = generic_args_of(&ty);
    assert!(
        matches!(&args[0], GenericArg::Type(Type::Named(path)) if path.head.as_ref() == "SIZE")
    );
}

#[test]
fn signed_bool_and_char_generic_arguments_parse() {
    let ty = param_type("f(x: Flags<-1, true, false, 'z'>) => void {}");
    let args = generic_args_of(&ty);
    assert!(matches!(
        args[0],
        GenericArg::Value(CompLiteral::Int { negative: true, .. })
    ));
    assert!(matches!(
        args[1],
        GenericArg::Value(CompLiteral::Bool(true))
    ));
    assert!(matches!(
        args[2],
        GenericArg::Value(CompLiteral::Bool(false))
    ));
    assert!(matches!(args[3], GenericArg::Value(CompLiteral::Char('z'))));
}

#[test]
fn nested_generic_argument_lists_parse() {
    let ty = param_type("f(x: Outer<Buffer<2, i32>, 3>) => void {}");
    let args = generic_args_of(&ty);
    let Some(inner) = args[0].as_type() else {
        panic!("expected a type argument");
    };
    assert_eq!(generic_args_of(inner).len(), 2);
    assert!(matches!(args[1], GenericArg::Value(_)));
}

#[test]
fn a_symbolic_array_length_parses_as_a_path() {
    let definition = struct_def("struct S<comp N: usize, T> { exposed data: [N]T; }");
    assert!(matches!(
        &definition.fields[0].r#type,
        Type::SizedArray(_, ArrayLength::Path(path)) if path.head.as_ref() == "N"
    ));
}

#[test]
fn a_literal_array_length_still_parses_as_a_literal() {
    let ty = param_type("f(x: [4]i32) => void {}");
    assert!(matches!(
        ty,
        Type::SizedArray(_, ArrayLength::Literal(CompLiteral::Int { .. }))
    ));
}

#[test]
fn a_value_generic_argument_commits_in_expression_position() {
    let function = function("f() => void { g<10, i32>(1); }");
    assert_eq!(function.codeblock.statements.len(), 1);
}

#[test]
fn a_comparison_chain_still_rolls_back_past_a_value_argument() {
    // `a < 10, i32 > b` is not a generic application: what follows `>` starts
    // a fresh operand, so the speculative parse must roll back.
    let function = function("f() => void { x := a < 10; y := 3 > b; }");
    assert_eq!(function.codeblock.statements.len(), 2);
}
