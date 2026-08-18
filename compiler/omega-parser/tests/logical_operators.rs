use omega_parser::SourceModule;
use omega_parser::prelude::{Expression, Item, LogicalOp, Statement};

fn bound_expression(body: &str) -> Expression {
    let source = format!("f() => void {{ v := {body}; }}");
    let module = SourceModule::parse(&source).unwrap_or_else(|e| panic!("`{body}`: {e:?}"));
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("expected a function");
    };
    let Statement::Walrus(w) = &f.codeblock.statements[0].statement else {
        panic!("expected a walrus binding");
    };
    w.value.expression.clone()
}

#[test]
fn bang_parses_as_a_prefix_operator() {
    assert!(matches!(bound_expression("!flag"), Expression::Not(_)));
    let Expression::Not(outer) = bound_expression("!!flag") else {
        panic!("expected the outer `!`");
    };
    assert!(matches!(outer.base.expression, Expression::Not(_)));
}

#[test]
fn bang_does_not_disturb_the_not_equal_token() {
    let Expression::BinaryOp(op) = bound_expression("a != b") else {
        panic!("expected `!=` to stay a comparison");
    };
    assert!(matches!(op.op, omega_parser::prelude::BinaryOp::Ne));
}

#[test]
fn logical_operators_parse_with_rust_precedence() {
    let Expression::Logical(or) = bound_expression("a || b && c") else {
        panic!("expected `||` at the root");
    };
    assert_eq!(or.op, LogicalOp::Or);
    let Expression::Logical(and) = &or.right.expression else {
        panic!("expected `&&` on the right of `||`");
    };
    assert_eq!(and.op, LogicalOp::And);
}

#[test]
fn comparison_binds_tighter_than_the_logical_operators() {
    let Expression::Logical(and) = bound_expression("a < b && c < d") else {
        panic!("expected `&&` at the root");
    };
    assert!(matches!(and.left.expression, Expression::BinaryOp(_)));
    assert!(matches!(and.right.expression, Expression::BinaryOp(_)));
}

#[test]
fn bitwise_and_binds_tighter_than_logical_and() {
    let Expression::Logical(and) = bound_expression("a & b && c") else {
        panic!("expected `&&` at the root");
    };
    assert!(matches!(and.left.expression, Expression::BinaryOp(_)));
}

#[test]
fn logical_operators_are_left_associative() {
    let Expression::Logical(outer) = bound_expression("a && b && c") else {
        panic!("expected `&&` at the root");
    };
    assert!(
        matches!(outer.left.expression, Expression::Logical(_)),
        "`a && b && c` must group as `(a && b) && c`"
    );
}

#[test]
fn address_of_a_pointer_still_parses() {
    SourceModule::parse("f() => void { x := 5; p := &x; q := &p; }")
        .expect("`&p` must still parse as address-of");
}
