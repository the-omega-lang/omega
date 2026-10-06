use super::*;
use crate::compiler_definitions::validate_literal;
use crate::target::{Arch, Os, Target};
use omega_parser::SourceModule;
use omega_parser::prelude::parse_literal;

fn definitions(options: &[&str]) -> CompilerDefinitions {
    definitions_for(Target::DEFAULT, options)
}

fn definitions_for(target: Target, options: &[&str]) -> CompilerDefinitions {
    let mut definitions = CompilerDefinitions::new(target);
    for option in options {
        let (name, value) = match option.split_once('=') {
            Some((name, text)) => {
                let literal = parse_literal(text).expect("the test literal parses");
                validate_literal(&literal, target.pointer_bits())
                    .expect("the test literal is a valid definition");
                (name, literal)
            }
            None => (*option, AnnotationLiteral::Bool(true)),
        };
        assert!(
            definitions.define(Ident(name.into()), value),
            "{name} is defined twice in one test configuration"
        );
    }
    definitions
}

fn item(condition: &str) -> omega_parser::prelude::ItemNode {
    let source = format!("@cond({condition})\nexposed a : i32 = 1;\n");
    let module = SourceModule::parse(&source)
        .unwrap_or_else(|errors| panic!("`{condition}` must parse: {errors:?}"));
    module.nodes.into_iter().next().expect("one item")
}

fn eval_with(definitions: &CompilerDefinitions, condition: &str) -> Result<bool, ConditionError> {
    item_is_enabled(definitions, &mut item(condition))
}

fn eval(condition: &str) -> bool {
    eval_with(&definitions(&[]), condition)
        .unwrap_or_else(|error| panic!("`{condition}` must evaluate: {error}"))
}

fn eval_defined(options: &[&str], condition: &str) -> bool {
    eval_with(&definitions(options), condition)
        .unwrap_or_else(|error| panic!("`{condition}` must evaluate: {error}"))
}

fn error(options: &[&str], condition: &str) -> String {
    eval_with(&definitions(options), condition)
        .map(|value| panic!("`{condition}` must fail, evaluated to {value}"))
        .unwrap_err()
        .to_string()
}

#[test]
fn a_literal_truth_decides_on_its_own() {
    assert!(eval("true"));
    assert!(!eval("false"));
}

#[test]
fn an_absent_definition_is_false_only_where_a_boolean_is_expected() {
    assert!(!eval("def::missing"));
    assert!(eval("not(def::missing)"));
    assert!(!eval("all(def::missing, true)"));
    assert!(eval("any(def::missing, true)"));
    assert!(error(&[], "equals(def::missing, 1)").contains("not defined"));
    assert!(error(&[], "equals(def::missing, false)").contains("not defined"));
}

#[test]
fn a_supplied_value_never_converts_to_a_truth() {
    assert!(eval_defined(&["flag"], "def::flag"));
    assert!(eval_defined(&["flag=false"], "not(def::flag)"));
    assert!(error(&["count=1"], "def::count").contains("expected a boolean"));
    assert!(error(&["label=\"x\""], "not(def::label)").contains("expected a boolean"));
    assert!(error(&[], "1").contains("expected a boolean"));
    assert!(error(&[], "\"yes\"").contains("expected a boolean"));
}

#[test]
fn empty_all_is_true_and_empty_any_is_false() {
    assert!(eval("all()"));
    assert!(!eval("any()"));
    assert!(eval("not(any())"));
}

#[test]
fn every_operand_is_checked_even_when_the_answer_is_already_decided() {
    assert!(error(&[], "any(true, equals(def::missing, 123))").contains("not defined"));
    assert!(error(&[], "all(false, bad_call())").contains("not a condition operator"));
    assert!(error(&[], "any(false, true, in(1, &[\"a\"]))").contains("cannot be compared"));
}

#[test]
fn integers_compare_mathematically_across_widths_and_signedness() {
    assert!(eval_defined(&["n=255u8"], "equals(def::n, 255)"));
    assert!(eval_defined(&["n=255u8"], "equals(255i64, def::n)"));
    assert!(eval_defined(&["n=-1i32"], "less(def::n, 0u64)"));
    assert!(eval_defined(
        &["n=18446744073709551615u64"],
        "greater(def::n, 0)"
    ));
    assert!(eval_defined(
        &["n=18446744073709551615u64"],
        "equals(def::n, 18446744073709551615)"
    ));
    assert!(eval_defined(&["n=-128i8"], "equals(def::n, -128)"));
}

#[test]
fn an_unsuffixed_literal_takes_its_counterparts_type_in_either_order() {
    // Without the counterpart's type this magnitude would not fit the `i32`
    // default, so both orders proving true is the whole point.
    assert!(eval_defined(
        &["n=4294967295u32"],
        "equals(def::n, 4294967295)"
    ));
    assert!(eval_defined(
        &["n=4294967295u32"],
        "equals(4294967295, def::n)"
    ));
    assert!(eval_defined(&["n=1.5f64"], "equals(def::n, 1.5)"));
    assert!(error(&[], "equals(4294967295, 1)").contains("does not fit"));
}

#[test]
fn a_float_compares_at_its_declared_width() {
    assert!(eval_defined(&["n=0.1"], "equals(def::n, 0.1)"));
    assert!(eval_defined(&["n=0.1f64"], "equals(def::n, 0.1f64)"));
    assert!(!eval_defined(&["n=0.1f64"], "equals(def::n, 0.1f32)"));
    assert!(eval_defined(&["n=-0.0"], "equals(def::n, 0.0)"));
    assert!(eval_defined(&["n=1.5"], "greater_equal(def::n, 1.5)"));
    assert!(eval_defined(&["n=1.5"], "less_equal(def::n, 2.0)"));
}

#[test]
fn text_and_character_values_compare_within_their_own_kind() {
    assert!(eval_defined(&["s=\"a\\tb\""], "equals(def::s, \"a\\tb\")"));
    assert!(eval_defined(&["c='\\n'"], "equals(def::c, '\\n')"));
    assert!(eval_defined(&["b=b\"raw\""], "equals(def::b, b\"raw\")"));
    assert!(error(&["s=\"a\""], "equals(def::s, 'a')").contains("cannot be compared"));
    assert!(error(&["s=\"raw\""], "equals(def::s, b\"raw\")").contains("cannot be compared"));
}

#[test]
fn mixed_kinds_never_compare() {
    assert!(error(&["n=1"], "equals(def::n, 1.0)").contains("cannot be compared"));
    assert!(error(&["n=1.0"], "equals(def::n, 1)").contains("cannot be compared"));
    assert!(error(&["n=1"], "equals(def::n, true)").contains("cannot be compared"));
    assert!(error(&["s=\"1\""], "less(def::s, \"2\")").contains("no ordering"));
    assert!(error(&[], "less(true, false)").contains("no ordering"));
    assert!(error(&["n=1"], "less(def::n, 1.0)").contains("cannot be compared"));
}

#[test]
fn membership_checks_every_element_against_the_first_operand() {
    assert!(eval("in(target_os, &[\"linux\", \"openbsd\"])"));
    assert!(!eval("in(target_os, &[\"windows\"])"));
    assert!(!eval("in(target_os, &[])"));
    assert!(eval_defined(&["n=200u8"], "in(def::n, &[100, 200])"));
    assert!(error(&[], "in(target_os, &[1])").contains("cannot be compared"));
    assert!(error(&[], "in(1, \"linux\")").contains("expected a list"));
    assert!(
        error(&[], "in(bad_name, &[1])").contains("not a compiler definition"),
        "the first operand is checked before an empty or matching list can decide"
    );
}

#[test]
fn a_boolean_call_is_also_a_value() {
    assert!(eval("equals(all(), true)"));
    assert!(eval("in(any(), &[false])"));
    assert!(eval_defined(&["flag"], "equals(def::flag, all(true))"));
}

#[test]
fn only_the_definition_namespace_and_the_builtin_names_resolve() {
    assert!(eval("equals(target_arch, \"x86_64\")"));
    assert!(eval("equals(target_pointer_width, 64)"));
    assert!(!eval("target_freestanding"));
    assert!(error(&[], "core::flag").contains("not a condition namespace"));
    assert!(error(&[], "flag").contains("not a compiler definition"));
    assert!(error(&["flag"], "flag").contains("not a compiler definition"));
    assert!(error(&[], "equals(target_env, 1)").contains("not a compiler definition"));
}

#[test]
fn builtins_follow_the_selected_target() {
    let freestanding = definitions_for(
        Target {
            arch: Arch::Thumbv7em,
            os: Os::None,
        },
        &[],
    );
    assert!(eval_with(&freestanding, "target_freestanding").unwrap());
    assert!(eval_with(&freestanding, "equals(target_pointer_width, 32)").unwrap());
    assert!(eval_with(&freestanding, "equals(target_os, \"none\")").unwrap());
    assert!(
        eval_with(&freestanding, "equals(1usize, 1)").unwrap(),
        "a usize literal decodes at the selected target's width"
    );
}

#[test]
fn an_unknown_operator_or_wrong_arity_is_rejected() {
    assert!(error(&[], "unless(true)").contains("not a condition operator"));
    assert!(error(&[], "not(true, false)").contains("'not' takes exactly one condition"));
    assert!(error(&[], "not()").contains("'not' takes exactly one condition"));
    assert!(error(&[], "equals(1)").contains("exactly two values"));
    assert!(error(&[], "equals(1, 2, 3)").contains("exactly two values"));
    assert!(error(&[], "in(1)").contains("a value and a list"));
}

#[test]
fn a_condition_must_be_exactly_one_positional_argument() {
    for malformed in [
        "@cond",
        "@cond()",
        "@cond(when = true)",
        "@cond(true, false)",
    ] {
        let source = format!("{malformed}\nexposed a : i32 = 1;\n");
        let module = SourceModule::parse(&source).expect("the annotation parses");
        let mut node = module.nodes.into_iter().next().expect("one item");
        let error = item_is_enabled(&definitions(&[]), &mut node)
            .expect_err(&format!("{malformed} must be rejected"));
        assert!(
            error.to_string().contains("exactly one condition"),
            "{malformed}: {error}"
        );
    }
}

#[test]
fn a_repeated_condition_is_rejected_before_it_is_evaluated() {
    let source = "@cond(false)\n@cond(equals(def::missing, 1))\nexposed a : i32 = 1;\n";
    let module = SourceModule::parse(source).expect("the annotations parse");
    let mut node = module.nodes.into_iter().next().expect("one item");
    let error = item_is_enabled(&definitions(&[]), &mut node)
        .expect_err("a repeated condition is an error");
    assert!(matches!(
        error.kind,
        ConditionErrorKind::DuplicateCondition { .. }
    ));
}

#[test]
fn evaluating_an_item_consumes_its_conditions() {
    let definitions = definitions(&["flag"]);
    let mut node = item("def::flag");
    assert!(item_is_enabled(&definitions, &mut node).expect("the condition evaluates"));
    assert!(
        node.conditions.is_empty(),
        "a retained item must not carry a condition any further"
    );
    assert!(
        item_is_enabled(&CompilerDefinitions::new(Target::DEFAULT), &mut node)
            .expect("a consumed condition cannot fail"),
        "re-filtering a retained item cannot change or re-evaluate the decision"
    );
}

#[test]
fn a_value_position_rejects_syntax_that_is_not_a_value() {
    assert!(error(&[], "equals(sizeof<u32>, 4)").contains("expected a value"));
    assert!(error(&[], "equals(&[1], &[1])").contains("expected a value"));
    assert!(error(&[], "&[true]").contains("expected a boolean"));
    assert!(error(&[], "sizeof<u32>").contains("expected a boolean"));
}

fn value_with(definitions: &CompilerDefinitions, expr: &str) -> Result<String, ConditionError> {
    let node = item(expr);
    let [AnnotationArg::Positional(expr)] = node.conditions[0].args.as_slice() else {
        panic!("one positional argument");
    };
    evaluate_value(definitions, expr).map(|literal| format!("{literal:?}"))
}

fn value(options: &[&str], expr: &str) -> String {
    value_with(&definitions(options), expr)
        .unwrap_or_else(|error| panic!("`{expr}` must evaluate: {error}"))
}

fn value_error(options: &[&str], expr: &str) -> String {
    value_with(&definitions(options), expr)
        .map(|value| panic!("`{expr}` must fail, evaluated to {value}"))
        .unwrap_err()
        .to_string()
}

fn literal(text: &str) -> String {
    format!(
        "{:?}",
        parse_literal(text).expect("the expected literal parses")
    )
}

#[test]
fn an_unsuffixed_definition_adapts_to_its_counterpart() {
    assert!(eval_defined(&["n=0.1"], "equals(def::n, 0.1f64)"));
    assert!(eval_defined(&["n=0.1"], "equals(0.1f64, def::n)"));
    assert!(eval_defined(
        &["n=4294967295"],
        "equals(def::n, 4294967295u32)"
    ));
    assert!(eval_defined(&["n=-128"], "equals(def::n, -128i8)"));
    assert!(
        error(&["n=300"], "equals(def::n, 1u8)").contains("does not fit"),
        "an unsuffixed definition is range-checked where it is used"
    );
    assert!(
        error(&["n=4294967295"], "equals(def::n, 1)").contains("does not fit"),
        "with no context an unsuffixed definition takes the ordinary default"
    );
}

#[test]
fn default_yields_the_definition_or_its_fallback() {
    assert!(eval_defined(
        &["buf=4096"],
        "greater(default(def::buf, 0), 1024)"
    ));
    assert!(!eval("greater(default(def::buf, 0), 1024)"));
    assert!(eval("equals(default(def::buf, 4294967295u32), 4294967295)"));
    assert!(
        eval_defined(
            &["buf=4294967295"],
            "equals(default(def::buf, 0u32), 4294967295)"
        ),
        "a suffixed fallback types an unsuffixed definition"
    );
    assert!(eval_defined(&["flag"], "default(def::flag, false)"));
    assert!(eval("default(def::flag, true)"));
    assert!(!eval("not(default(def::flag, true))"));
}

#[test]
fn default_checks_its_operands_whichever_is_chosen() {
    assert!(error(&[], "equals(default(target_os, \"x\"), \"x\")").contains("first operand"));
    assert!(error(&[], "equals(default(core::x, 1), 1)").contains("not a condition namespace"));
    assert!(error(&[], "equals(default(def::x, def::y), 1)").contains("fallback"));
    assert!(error(&["n=5"], "equals(default(def::n, \"x\"), \"x\")").contains("same kind"));
    assert!(error(&["n=5"], "equals(default(def::n, 1.0), 1.0)").contains("same kind"));
    assert!(error(&["n=5"], "equals(default(def::n, 300u8), 5)").contains("does not fit"));
    assert!(error(&[], "default(def::n)").contains("a definition and a literal"));
    assert!(error(&[], "default(def::n, 1)").contains("expected a boolean"));
}

#[test]
fn default_checks_the_definition_before_the_fallback() {
    assert!(
        error(&["n=300"], "equals(default(def::n, 256u8), 0)").contains("'300' does not fit"),
        "both operands fail in the fallback's u8 context, but the definition is first"
    );
}

#[test]
fn a_value_is_returned_as_written() {
    assert_eq!(value(&["n=4096"], "def::n"), literal("4096"));
    assert_eq!(value(&["n=-128"], "def::n"), literal("-128"));
    assert_eq!(value(&["n=1.5f64"], "def::n"), literal("1.5f64"));
    assert_eq!(value(&["s=\"x\""], "def::s"), literal("\"x\""));
    assert_eq!(value(&["flag"], "def::flag"), literal("true"));
    assert_eq!(value(&[], "default(def::n, 7)"), literal("7"));
    assert_eq!(value(&["n=9"], "default(def::n, 7)"), literal("9"));
    assert_eq!(value(&[], "300"), literal("300"));
}

#[test]
fn a_builtin_value_keeps_its_declared_type() {
    assert_eq!(value(&[], "target_pointer_width"), literal("64u32"));
    assert_eq!(value(&[], "target_os"), literal("\"linux\""));
    assert_eq!(value(&[], "target_freestanding"), literal("false"));
}

#[test]
fn an_operator_call_is_a_boolean_value() {
    assert_eq!(value(&[], "equals(target_os, \"linux\")"), literal("true"));
    assert_eq!(value(&[], "not(def::flag)"), literal("true"));
    assert_eq!(value(&["n=5"], "greater(def::n, 9)"), literal("false"));
}

#[test]
fn a_value_position_needs_a_supplied_definition() {
    assert!(value_error(&[], "def::missing").contains("not defined"));
    assert!(value_error(&[], "equals(def::missing, 1)").contains("not defined"));
    assert!(value_error(&[], "&[1]").contains("expected a value"));
    assert!(value_error(&[], "core::x").contains("not a condition namespace"));
    assert!(value_error(&[], "default(def::n, 300u8)").contains("does not fit"));
    assert!(
        value_error(&["n=5"], "default(def::n, 300u8)").contains("does not fit"),
        "the fallback is checked even when the definition is chosen"
    );
}
