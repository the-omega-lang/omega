use omega_parser::prelude::{Expression, Item, Statement, StatementNode, Type};
use omega_parser::SourceModule;

fn expression_statement(source: &str) -> Expression {
    let module = SourceModule::parse(source).unwrap();
    let Item::FunctionDefinition(function) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected function definition");
    };
    if let Some(tail) = function.codeblock.tail {
        return tail.expression;
    }
    let StatementNode { statement, .. } =
        function.codeblock.statements.into_iter().last().unwrap();
    let Statement::Expression(expression) = statement else {
        panic!("expected an expression statement");
    };
    expression.expression
}

fn call_callee(source: &str) -> Expression {
    let Expression::FunctionCall(call) = expression_statement(source) else {
        panic!("expected a function call");
    };
    call.callee.expression
}

fn named(head: &str, ty: &Type) -> bool {
    matches!(ty, Type::Named(path) if path.head.as_ref() == head && path.tail.is_empty())
}

#[test]
fn fully_qualified_spec_path_parses_as_a_path_with_a_qualifying_pair() {
    let Expression::Path(path) = call_callee("f() => void { <S : P>::make() }") else {
        panic!("the qualified callee must be an expression path");
    };
    let qualified = path
        .qualified_spec
        .as_ref()
        .expect("the path carries its qualifying pair");
    assert_eq!(path.path.head.as_ref(), "make");
    assert!(path.path.tail.is_empty());
    assert!(path.generic_args.is_empty());
    assert!(named("S", &qualified.target));
    assert!(named("P", &qualified.spec));
}

#[test]
fn fully_qualified_spec_path_with_a_generic_spec_parses() {
    let Expression::Path(path) = call_callee("f() => void { <S : P<i32>>::make() }") else {
        panic!("the qualified callee must be an expression path");
    };
    let qualified = path
        .qualified_spec
        .as_ref()
        .expect("the path carries its qualifying pair");
    assert!(named("S", &qualified.target));
    assert!(matches!(
        &qualified.spec,
        Type::Generic(path, args) if path.head.as_ref() == "P" && args.len() == 1
    ));
}

#[test]
fn fully_qualified_spec_path_with_a_qualified_target_parses() {
    let Expression::Path(path) = call_callee("f() => void { <mod::S : P>::make() }") else {
        panic!("the qualified callee must be an expression path");
    };
    let qualified = path
        .qualified_spec
        .as_ref()
        .expect("the path carries its qualifying pair");
    assert!(matches!(
        &qualified.target,
        Type::Named(path) if path.head.as_ref() == "mod" && path.tail.len() == 1
    ));
}

#[test]
fn a_plain_cast_is_still_a_cast_not_a_path() {
    assert!(matches!(
        expression_statement("f() => void { <i32>x }"),
        Expression::Cast(_)
    ));
}

#[test]
fn a_cast_whose_target_type_is_generic_is_still_a_cast() {
    assert!(matches!(
        expression_statement("f() => void { <*mut Node<T>>p }"),
        Expression::Cast(_)
    ));
}

#[test]
fn a_qualified_spec_path_can_be_called_with_arguments() {
    let Expression::FunctionCall(call) =
        expression_statement("f() => void { <S : P>::combine(a, b) }")
    else {
        panic!("expected a function call");
    };
    let Expression::Path(path) = call.callee.expression else {
        panic!("the qualified callee must be an expression path");
    };
    assert_eq!(path.path.head.as_ref(), "combine");
    assert_eq!(call.args.len(), 2);
}
