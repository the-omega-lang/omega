use omega_parser::SourceModule;
use omega_parser::prelude::{
    AliasTarget, Expression, Item, MatchArm, ParseErrorKind, PatternValue, Type,
};

fn alias_target(source: &str) -> Type {
    let module = SourceModule::parse(source).expect("alias source must parse");
    let Item::Alias(alias) = &module.nodes.last().expect("at least one item").item else {
        panic!("expected an alias item");
    };
    match &alias.target {
        AliasTarget::Type(ty) => ty.clone(),
        other => panic!("expected a type target, found {other:?}"),
    }
}

fn members(ty: &Type) -> &[Type] {
    let Type::AnonymousEnum(members) = ty else {
        panic!("expected an anonymous enum type, found {ty:?}");
    };
    members
}

fn first_param_type(source: &str) -> Type {
    let module = SourceModule::parse(source).expect("source must parse");
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("first item must be a function");
    };
    f.params[0].r#type.clone()
}

fn match_arms(source: &str) -> Vec<MatchArm> {
    let module = SourceModule::parse(source).expect("source must parse");
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("first item must be a function");
    };
    let tail = f
        .codeblock
        .tail
        .as_ref()
        .expect("the match is the block's tail expression");
    let Expression::Match(m) = &tail.expression else {
        panic!("expected a match expression");
    };
    m.arms.clone()
}

fn error_kinds(source: &str) -> Vec<ParseErrorKind> {
    SourceModule::parse(source)
        .err()
        .expect("expected this source to be rejected")
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn one_member_is_a_legal_anonymous_enum() {
    let ty = alias_target("alias A = enum i32;");
    assert_eq!(members(&ty), &[Type::Named(path("i32"))]);
}

#[test]
fn members_keep_their_written_order_through_parsing() {
    // Canonical ordering is semantic; the parser must not reorder or dedup.
    let ty = alias_target("alias A = enum i32 | *str | i32;");
    assert_eq!(members(&ty).len(), 3);
    assert!(matches!(members(&ty)[1], Type::Pointer(_, false)));
}

#[test]
fn a_member_may_be_any_type_form() {
    let ty = alias_target("alias A = enum *mut Failure | [4]u8 | Holder<i32> | (x: i32) => bool;");
    let members = members(&ty);
    assert_eq!(members.len(), 4);
    assert!(matches!(members[0], Type::Pointer(_, true)));
    assert!(matches!(members[1], Type::SizedArray(_, _)));
    assert!(matches!(members[2], Type::Generic(_, _)));
    assert!(matches!(members[3], Type::Function(_)));
}

#[test]
fn a_nested_anonymous_member_consumes_the_rest_of_the_list() {
    // There is no parenthesized type syntax, so `enum` inside a member list
    // nests to the right rather than continuing the outer list. This nesting
    // is source structure only: semantic resolution flattens it away, so the
    // parser must not be taught to flatten it here.
    let ty = alias_target("alias A = enum C | enum A | B;");
    let members = members(&ty);
    assert_eq!(members.len(), 2);
    assert_eq!(self::members(&members[1]).len(), 2);
}

#[test]
fn an_anonymous_enum_is_legal_in_a_signature_and_behind_a_pointer() {
    assert!(matches!(
        first_param_type("f(v: enum i32 | *str) => void { }"),
        Type::AnonymousEnum(_)
    ));
    let Type::Pointer(pointee, false) = first_param_type("f(v: *enum i32 | *str) => void { }")
    else {
        panic!("expected a pointer type");
    };
    assert!(matches!(*pointee, Type::AnonymousEnum(_)));
}

#[test]
fn a_member_list_ends_where_a_type_can_no_longer_continue() {
    let ty = first_param_type("f(v: Holder<enum i32 | bool>) => void { }");
    let Type::Generic(_, args) = ty else {
        panic!("expected a generic type");
    };
    assert_eq!(
        members(args[0].as_type().expect("a type argument")).len(),
        2
    );
}

#[test]
fn a_missing_member_after_the_bar_is_a_parse_error() {
    let kinds = error_kinds("alias A = enum i32 | ;");
    assert!(matches!(kinds[0], ParseErrorKind::Expected { .. }));
}

#[test]
fn a_type_shaped_arm_keeps_both_readings() {
    let arms = match_arms("f(v: T) => void { match v { A => 1, *str => 2, } }");
    // `A` is ambiguous: the analyzer decides between the two readings from
    // the scrutinee's type, so both must survive parsing.
    assert!(arms[0].pattern.value.is_some());
    assert!(matches!(arms[0].pattern.r#type, Some(Type::Named(_))));
    assert!(matches!(
        arms[1].pattern.r#type,
        Some(Type::Pointer(_, false))
    ));
}

#[test]
fn a_type_only_arm_still_parses() {
    // `[4]u8` and `Holder<i32>` have no value reading at all.
    let arms = match_arms("f(v: T) => void { match v { [4]u8 => 1, Holder<i32> => 2, } }");
    assert!(arms[0].pattern.value.is_none());
    assert!(matches!(
        arms[0].pattern.r#type,
        Some(Type::SizedArray(_, _))
    ));
    assert!(arms[1].pattern.value.is_none());
    assert!(matches!(arms[1].pattern.r#type, Some(Type::Generic(_, _))));
}

#[test]
fn ordinary_patterns_keep_their_value_reading() {
    let arms = match_arms(
        "f(v: T) => void { match v { Enum::Variant => 1, 0..<10 => 2, 'a' => 3, .. => 4, } }",
    );
    assert!(matches!(
        arms[0].pattern.value,
        Some(PatternValue::Value(_))
    ));
    // A variant path is also a legal type spelling, so the candidate is kept;
    // the analyzer only prefers it for an anonymous-enum scrutinee.
    assert!(arms[0].pattern.r#type.is_some());

    assert!(matches!(
        arms[1].pattern.value,
        Some(PatternValue::Range(_))
    ));
    assert!(arms[1].pattern.r#type.is_none());
    assert!(matches!(
        arms[2].pattern.value,
        Some(PatternValue::Value(_))
    ));
    assert!(arms[2].pattern.r#type.is_none());
    assert!(matches!(
        arms[3].pattern.value,
        Some(PatternValue::Range(_))
    ));
    assert!(arms[3].pattern.r#type.is_none());
}

fn path(name: &str) -> omega_parser::prelude::Path {
    omega_parser::prelude::Path {
        anchor: None,
        head: omega_parser::prelude::Ident(name.to_string()),
        tail: Vec::new(),
        origin: omega_parser::prelude::Origin::default(),
    }
}
