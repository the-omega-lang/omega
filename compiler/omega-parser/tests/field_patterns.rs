use omega_parser::SourceModule;
use omega_parser::prelude::{
    Expression, FieldsPattern, Item, MatchArm, MatchExpr, ParseErrorKind, Pattern, PatternValue,
    Type,
};

fn function_tail(source: &str) -> Expression {
    let module = SourceModule::parse(source).expect("source must parse");
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("first item must be a function");
    };
    f.codeblock
        .as_ref()
        .unwrap()
        .tail
        .as_ref()
        .expect("the function has a tail expression")
        .expression
        .clone()
}

fn match_of(expression: &Expression) -> &MatchExpr {
    let Expression::Match(m) = expression else {
        panic!("expected a match expression, found {expression:?}");
    };
    m
}

fn match_arms(source: &str) -> Vec<MatchArm> {
    match_of(&function_tail(source)).arms.clone()
}

fn fields(pattern: &Pattern) -> &FieldsPattern {
    match &pattern.value {
        Some(PatternValue::Fields(fields)) => fields,
        other => panic!("expected a field pattern, found {other:?}"),
    }
}

fn field_names(fields: &FieldsPattern) -> Vec<&str> {
    fields.fields.iter().map(|f| f.name.as_ref()).collect()
}

#[test]
fn nested_field_patterns() {
    let arms = match_arms(
        "f(e: Event) => void { match e { \
            Event::Click { pos = Point { x = 0; .. }; button = Button::Left; } => 1, \
         } }",
    );
    let outer = fields(&arms[0].pattern);
    assert_eq!(outer.path.path.tail[0].as_ref(), "Click");
    assert_eq!(field_names(outer), ["pos", "button"]);
    assert!(outer.rest.is_none());
    assert!(arms[0].pattern.r#type.is_none());

    let pos = fields(&outer.fields[0].pattern);
    assert_eq!(pos.path.path.head.as_ref(), "Point");
    assert_eq!(field_names(pos), ["x"]);
    assert!(pos.rest.is_some());
    assert!(matches!(
        pos.fields[0].pattern.value,
        Some(PatternValue::Value(_))
    ));

    let button = &outer.fields[1].pattern;
    assert!(matches!(button.value, Some(PatternValue::Value(_))));
    // `;` ends a subpattern the way `=>` ends an arm, so the type reading
    // survives for an anonymous-enum position.
    assert!(button.r#type.is_some());
}

#[test]
fn rest_alone_and_ranges_in_subpatterns() {
    let arms = match_arms(
        "f(p: Point) => void { match p { \
            Point { .. } => 1, \
            Point { x = 0..<10; y = ..; } => 2, \
         } }",
    );
    let any = fields(&arms[0].pattern);
    assert!(any.fields.is_empty());
    assert!(any.rest.is_some());

    let ranged = fields(&arms[1].pattern);
    assert!(matches!(
        ranged.fields[0].pattern.value,
        Some(PatternValue::Range(_))
    ));
    assert!(matches!(
        ranged.fields[1].pattern.value,
        Some(PatternValue::Range(_))
    ));
}

#[test]
fn anonymous_member_type_at_a_nested_position() {
    let arms = match_arms(
        "f(w: Wrapper) => void { match w { Wrapper { value = *str; } => 1, \
                                           Wrapper { value = [4]u8; } => 2, } }",
    );
    let value = &fields(&arms[0].pattern).fields[0].pattern;
    assert!(matches!(value.r#type, Some(Type::Pointer(..))));
    let value = &fields(&arms[1].pattern).fields[0].pattern;
    assert!(value.value.is_none());
    assert!(matches!(value.r#type, Some(Type::SizedArray(..))));
}

#[test]
fn field_patterns_inside_a_restricted_condition() {
    let tail = function_tail(
        "f(p: Point) => bool { \
            if match p { Point { x = 0; .. } => true, .. => false } { true } else { false } \
         }",
    );
    let Expression::If(if_expr) = tail else {
        panic!("expected an if expression");
    };
    let condition = &if_expr.branches[0].0;
    let arms = &match_of(&condition.expression).arms;
    assert_eq!(field_names(fields(&arms[0].pattern)), ["x"]);
}

#[test]
fn field_pattern_as_a_match_scrutinee_position_is_still_restricted() {
    // In the scrutinee itself, `p { ... }` is not a pattern: the brace opens
    // the arm list.
    let arms = match_arms("f(p: Point) => void { match p { Point { .. } => 1 } }");
    assert_eq!(arms.len(), 1);
}

#[test]
fn a_subpattern_needs_its_semicolon() {
    let errors = SourceModule::parse("f(p: Point) => void { match p { Point { x = 0 } => 1 } }")
        .err()
        .expect("a field subpattern without ';' is rejected");
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        ParseErrorKind::Expected {
            expected: "';'",
            ..
        }
    )));
}
