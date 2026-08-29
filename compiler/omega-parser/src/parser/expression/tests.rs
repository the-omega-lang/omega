use crate::SourceModule;
use crate::ast::expression::Expression;
use crate::ast::item::Item;
use crate::ast::statement::Statement;
use crate::diagnostics::ParseErrorKind;

fn body_statements(source: &str) -> Vec<Statement> {
    let module = SourceModule::parse(source).expect("source must parse");
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("first item must be a function");
    };
    f.codeblock
        .statements
        .iter()
        .map(|s| s.statement.clone())
        .collect()
}

#[test]
fn struct_literal_parses_with_fields_in_order() {
    let stmts = body_statements("f() => i32 { v := Vec2 { x = 1; y = 2; }; v.x }");
    let Statement::Walrus(w) = &stmts[0] else {
        panic!("expected a walrus statement")
    };
    let Expression::StructLiteral(lit) = &w.value.expression else {
        panic!("expected a struct literal value")
    };
    assert_eq!(lit.path.path.head.as_ref(), "Vec2");
    let names: Vec<&str> = lit.fields.iter().map(|f| f.name.as_ref()).collect();
    assert_eq!(names, ["x", "y"]);
}

#[test]
fn generic_args_commit_on_path_continuation() {
    let stmts = body_statements("f() => void { a := Optional<u32>::Some { value = 10; }; }");
    let Statement::Walrus(w) = &stmts[0] else {
        panic!("expected a walrus statement")
    };
    let Expression::StructLiteral(lit) = &w.value.expression else {
        panic!("expected a struct literal value")
    };
    assert_eq!(lit.path.path.head.as_ref(), "Optional");
    assert_eq!(lit.path.path.tail[0].as_ref(), "Some");
    assert_eq!(lit.path.generic_args.len(), 1);
    assert_eq!(lit.path.args_at, 0);
}

#[test]
fn generic_args_do_not_steal_comparisons() {
    let stmts = body_statements("f() => void { x := a < b; g(a < b, c > d); }");
    let Statement::Walrus(w) = &stmts[0] else {
        panic!("expected a walrus statement")
    };
    assert!(matches!(w.value.expression, Expression::BinaryOp(_)));
    let Statement::Expression(call) = &stmts[1] else {
        panic!("expected a call statement")
    };
    let Expression::FunctionCall(call) = &call.expression else {
        panic!("expected a call")
    };
    assert_eq!(call.args.len(), 2);
}

#[test]
fn generic_args_commit_before_a_call_suffix() {
    let stmts = body_statements("f() => void { g<i32>(); }");
    let Statement::Expression(expr) = &stmts[0] else {
        panic!("expected an expression statement")
    };
    let Expression::FunctionCall(call) = &expr.expression else {
        panic!("expected a call")
    };
    let Expression::Path(path) = &call.callee.expression else {
        panic!("expected a path callee")
    };
    assert_eq!(path.path.head.as_ref(), "g");
    assert_eq!(path.generic_args.len(), 1);
    assert_eq!(path.args_at, 0);
    assert!(call.args.is_empty());
}

#[test]
fn generic_args_commit_on_a_module_qualified_call() {
    let stmts = body_statements("f() => void { mem::ptr_cast<u32>(p); }");
    let Statement::Expression(expr) = &stmts[0] else {
        panic!("expected an expression statement")
    };
    let Expression::FunctionCall(call) = &expr.expression else {
        panic!("expected a call")
    };
    let Expression::Path(path) = &call.callee.expression else {
        panic!("expected a path callee")
    };
    assert_eq!(path.path.head.as_ref(), "mem");
    assert_eq!(path.path.tail[0].as_ref(), "ptr_cast");
    assert_eq!(path.generic_args.len(), 1);
    assert_eq!(path.args_at, 1);
    assert_eq!(call.args.len(), 1);
}

#[test]
fn nested_generic_args_close_a_call_application() {
    let stmts = body_statements("f() => void { g<Pair<i32, u32>>(); }");
    let Statement::Expression(expr) = &stmts[0] else {
        panic!("expected an expression statement")
    };
    let Expression::FunctionCall(call) = &expr.expression else {
        panic!("expected a call")
    };
    let Expression::Path(path) = &call.callee.expression else {
        panic!("expected a path callee")
    };
    assert_eq!(path.generic_args.len(), 1);
}

#[test]
fn generic_args_commit_at_an_expression_boundary() {
    let stmts = body_statements("f() => void { p := &foo<i32>; a := [foo<i32>, bar<i32>]; }");
    let Statement::Walrus(w) = &stmts[0] else {
        panic!("expected a walrus statement")
    };
    let Expression::AddressOf(address) = &w.value.expression else {
        panic!("expected an address-of expression")
    };
    let Expression::Path(path) = &address.base.expression else {
        panic!("expected a path operand")
    };
    assert_eq!(path.path.head.as_ref(), "foo");
    assert_eq!(path.generic_args.len(), 1);

    let Statement::Walrus(w) = &stmts[1] else {
        panic!("expected a walrus statement")
    };
    let Expression::ArrayLiteral(array) = &w.value.expression else {
        panic!("expected an array literal")
    };
    assert_eq!(array.elements.len(), 2);
    for element in &array.elements {
        let Expression::Path(path) = &element.expression else {
            panic!("expected a path element")
        };
        assert_eq!(path.generic_args.len(), 1);
    }
}

#[test]
fn a_generic_call_commits_before_any_semantic_lookup() {
    let stmts = body_statements("f() => void { a<b>(c); }");
    let Statement::Expression(expr) = &stmts[0] else {
        panic!("expected an expression statement")
    };
    let Expression::FunctionCall(call) = &expr.expression else {
        panic!("expected a call, not a comparison")
    };
    let Expression::Path(path) = &call.callee.expression else {
        panic!("expected a path callee")
    };
    assert_eq!(path.path.head.as_ref(), "a");
    assert_eq!(path.generic_args.len(), 1);
}

#[test]
fn a_fresh_operand_after_the_close_angle_stays_a_chained_comparison() {
    let errors =
        SourceModule::parse("f() => void { x := a < b > c; }").expect_err("must not parse");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, ParseErrorKind::ChainedComparison))
    );
}

#[test]
fn enum_with_header_bodies_and_functions_parses() {
    let source = r#"
        enum MyCoolEnum(tag: i16, description: *u8) {
            Bad(-1, "bad"),
            First(0, "first") { message: *u8; },
            Second(1, "second") {
                number: u64;
                decimal: f64;
            }
            Third(2, "third");

            print_description(self) => void { puts(self.description); }
            make() => MyCoolEnum { MyCoolEnum::Third }
        }
    "#;
    let module = SourceModule::parse(source).expect("enum must parse");
    let Item::Enum(e) = &module.nodes[0].item else {
        panic!("expected an enum item")
    };
    assert_eq!(e.ident.as_ref(), "MyCoolEnum");
    assert_eq!(e.header.len(), 2);
    assert_eq!(e.header[0].ident.as_ref(), "tag");
    let names: Vec<&str> = e.variants.iter().map(|v| v.ident.as_ref()).collect();
    assert_eq!(names, ["Bad", "First", "Second", "Third"]);
    assert_eq!(e.variants[2].fields.len(), 2);
    assert_eq!(e.functions.len(), 2);
}

#[test]
fn enum_function_without_variant_terminator_reports_dedicated_error() {
    let errors = SourceModule::parse("enum E { First, Second do_thing(self) => void { } }")
        .expect_err("must not parse");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, ParseErrorKind::EnumFunctionBeforeSemi))
    );
}

#[test]
fn enum_in_statement_position_reports_dedicated_error() {
    let errors = SourceModule::parse("f() => void { enum E { A } }").expect_err("must not parse");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e.kind, ParseErrorKind::EnumNotAllowedHere))
    );
}

#[test]
fn condition_position_reads_brace_as_body_not_literal() {
    let stmts = body_statements("f() => void { while flag { x: i32; } }");
    let Statement::While(w) = &stmts[0] else {
        panic!("expected a while statement")
    };
    assert!(matches!(w.condition.expression, Expression::Path(_)));
    assert!(matches!(
        w.body.statements[0].statement,
        Statement::Declaration(_)
    ));
}

#[test]
fn unambiguous_literal_in_condition_reports_dedicated_error() {
    let errors = SourceModule::parse("f() => void { if Vec2 { x = 1; }.x > 0 { g(); } }")
        .expect_err("must not parse");
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        errors[0].kind,
        ParseErrorKind::StructLiteralNotAllowedHere
    ));
}

#[test]
fn parenthesized_literal_in_condition_parses() {
    let stmts = body_statements("f() => void { if (Vec2 { x = 1; }).x > 0 { g(); } done(); }");
    assert!(matches!(stmts[0], Statement::Expression(_)));
    assert_eq!(stmts.len(), 2);
}

#[test]
fn literal_inside_call_arguments_in_condition_parses() {
    let stmts = body_statements("f() => void { if check(Vec2 { x = 1; }) { g(); } done(); }");
    assert!(matches!(stmts[0], Statement::Expression(_)));
    assert_eq!(stmts.len(), 2);
}

fn shape(expr: &Expression) -> String {
    match expr {
        Expression::BinaryOp(b) => format!(
            "({} {:?} {})",
            shape(&b.left.expression),
            b.op,
            shape(&b.right.expression)
        ),
        Expression::Path(p) => p.path.head.as_ref().to_string(),
        other => format!("{other:?}"),
    }
}

fn bound_shape(value: &str) -> String {
    let stmts = body_statements(&format!("f() => void {{ v := {value}; }}"));
    let Statement::Walrus(w) = &stmts[0] else {
        panic!("expected a walrus statement")
    };
    shape(&w.value.expression)
}

#[test]
fn binary_tiers_group_loosest_to_tightest() {
    assert_eq!(
        bound_shape("a | b ^ c & d << e + f * g"),
        "(a BitOr (b BitXor (c BitAnd (d Shl (e Add (f Mul g))))))"
    );
}

#[test]
fn comparison_is_looser_than_the_bitwise_tiers() {
    assert_eq!(bound_shape("a & b == c"), "((a BitAnd b) Eq c)");
}

#[test]
fn binary_tiers_are_left_associative() {
    assert_eq!(bound_shape("a - b - c"), "((a Sub b) Sub c)");
}

#[test]
fn comp_and_reveal_accept_a_cast_operand() {
    for word in ["comp", "reveal"] {
        let stmts = body_statements(&format!("f() => void {{ v := {word} <i32>5; }}"));
        let Statement::Walrus(w) = &stmts[0] else {
            panic!("expected a walrus statement")
        };
        let inner = match &w.value.expression {
            Expression::Comp(c) => &c.base.expression,
            Expression::Reveal(r) => &r.base.expression,
            other => panic!("`{word} <i32>5` must stay a prefix operator, got {other:?}"),
        };
        assert!(
            matches!(inner, Expression::Cast(_)),
            "`{word}`'s operand must be the cast"
        );
    }
}
