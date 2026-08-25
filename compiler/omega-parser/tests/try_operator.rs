use omega_parser::prelude::{BinaryOp, Expression, ExpressionNode, Item, Statement, TryExpr};
use omega_parser::{SourceModule, macros};
use std::collections::HashMap;

/// The tail expression of `f() => T { <expr> }`, so each case reads as the
/// expression under test rather than as item scaffolding.
fn tail_expression(expr_source: &str) -> ExpressionNode {
    let source = format!("f() => i32 {{ {expr_source} }}");
    let module = SourceModule::parse(&source).expect("test source must parse");
    let Item::FunctionDefinition(f) = &module.nodes.last().expect("one item").item else {
        panic!("expected a function definition");
    };
    *f.codeblock
        .tail
        .clone()
        .expect("expected a tail expression")
}

fn expect_try(node: &ExpressionNode) -> &TryExpr {
    match &node.expression {
        Expression::Try(t) => t,
        other => panic!("expected a try expression, found {other:?}"),
    }
}

#[test]
fn a_call_result_is_the_try_operand() {
    let expr = tail_expression("call()?");
    let r#try = expect_try(&expr);
    assert!(matches!(r#try.base.expression, Expression::FunctionCall(_)));
}

#[test]
fn the_operator_span_covers_only_the_question_mark() {
    let source = "f() => i32 { call()? }";
    let module = SourceModule::parse(source).expect("test source must parse");
    let Item::FunctionDefinition(f) = &module.nodes.last().expect("one item").item else {
        panic!("expected a function definition");
    };
    let expr = f.codeblock.tail.clone().expect("a tail expression");
    let r#try = expect_try(&expr);
    assert_eq!(
        &source[r#try.operator_span.start..r#try.operator_span.end],
        "?"
    );
    assert_eq!(
        &source[expr.span.start..expr.span.end],
        "call()?",
        "the whole expression span still covers operand and operator"
    );
}

#[test]
fn field_access_applies_to_the_try_result() {
    let expr = tail_expression("value?.field");
    let Expression::FieldAccess(access) = &expr.expression else {
        panic!("expected the field access to be outermost");
    };
    assert_eq!(access.field.as_ref(), "field");
    expect_try(&access.base);
}

#[test]
fn try_chains_left_to_right() {
    let expr = tail_expression("nested??");
    let outer = expect_try(&expr);
    let inner = expect_try(&outer.base);
    assert!(matches!(inner.base.expression, Expression::Path(_)));
}

#[test]
fn a_call_can_be_applied_to_a_try_result() {
    let expr = tail_expression("fallible?()");
    let Expression::FunctionCall(call) = &expr.expression else {
        panic!("expected the call to be outermost");
    };
    expect_try(&call.callee);
}

#[test]
fn indexing_can_be_applied_to_a_try_result() {
    let expr = tail_expression("fallible?[0]");
    let Expression::Index(index) = &expr.expression else {
        panic!("expected the index to be outermost");
    };
    expect_try(&index.base);
}

#[test]
fn try_binds_tighter_than_a_binary_operator() {
    let expr = tail_expression("a? + b");
    let Expression::BinaryOp(bin) = &expr.expression else {
        panic!("expected the addition to be outermost");
    };
    assert_eq!(bin.op, BinaryOp::Add);
    expect_try(&bin.left);
    assert!(matches!(bin.right.expression, Expression::Path(_)));
}

#[test]
fn try_binds_tighter_than_a_unary_operator() {
    let expr = tail_expression("-a?");
    let Expression::Negate(negate) = &expr.expression else {
        panic!("expected the negation to be outermost");
    };
    expect_try(&negate.base);
}

#[test]
fn a_try_operand_survives_macro_expansion() {
    let source = "macro try_it($e: expr) => { $e? } main() => void { x := try_it$(call()); }";
    let expanded = macros::expand(SourceModule::parse(source).unwrap(), &HashMap::new()).unwrap();
    let Item::FunctionDefinition(f) = &expanded.nodes.last().expect("one item").item else {
        panic!("expected a function definition");
    };
    let Statement::Walrus(walrus) = &f.codeblock.statements[0].statement else {
        panic!("expected the walrus declaration");
    };
    let r#try = expect_try(&walrus.value);
    assert!(
        matches!(r#try.base.expression, Expression::FunctionCall(_)),
        "the expanded operand must replace the metavariable"
    );
}
