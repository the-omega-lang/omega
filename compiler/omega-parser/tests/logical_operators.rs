//! `!`, `&&` and `||`: the grammar side. Semantics (both operands must be
//! `bool`) and the short-circuit itself are the analyzer's, and are covered
//! end-to-end by the compiled gates.

use omega_parser::SourceModule;
use omega_parser::prelude::{Expression, Item, LogicalOp, Statement};

/// The single expression `f`'s body binds with `:=`.
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
    // Stacking is fine, and right-associative like every other prefix.
    let Expression::Not(outer) = bound_expression("!!flag") else {
        panic!("expected the outer `!`");
    };
    assert!(matches!(outer.base.expression, Expression::Not(_)));
}

#[test]
fn bang_does_not_disturb_the_not_equal_token() {
    // `!=` must still lex as one token -- maximal munch puts it ahead of
    // the new single-character `!`.
    let Expression::BinaryOp(op) = bound_expression("a != b") else {
        panic!("expected `!=` to stay a comparison");
    };
    assert!(matches!(
        op.op,
        omega_parser::prelude::BinaryOp::Ne
    ));
}

#[test]
fn logical_operators_parse_with_rust_precedence() {
    // `&&` binds tighter than `||`.
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
    // The whole reason these tiers sit between assignment and comparison:
    // `a < b && c < d` must need no parentheses.
    let Expression::Logical(and) = bound_expression("a < b && c < d") else {
        panic!("expected `&&` at the root");
    };
    assert!(matches!(and.left.expression, Expression::BinaryOp(_)));
    assert!(matches!(and.right.expression, Expression::BinaryOp(_)));
}

#[test]
fn bitwise_and_binds_tighter_than_logical_and() {
    // `&` and `&&` coexist: `a & b && c` is `(a & b) && c`.
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
    // `&&` is one token now, so a doubled address-of needs the space --
    // but taking the address of a pointer *variable* (the common shape) is
    // untouched.
    SourceModule::parse("f() => void { x := 5; p := &x; q := &p; }")
        .expect("`&p` must still parse as address-of");
}
