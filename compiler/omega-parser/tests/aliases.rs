use omega_parser::SourceModule;
use omega_parser::prelude::{AliasTarget, Item, ParseErrorKind, Type, Visibility};

fn alias(source: &str) -> omega_parser::prelude::AliasItem {
    let module = SourceModule::parse(source).expect("alias source must parse");
    match &module.nodes.last().expect("at least one item").item {
        Item::Alias(alias) => alias.clone(),
        other => panic!("expected an alias item, found {other:?}"),
    }
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
fn a_bare_name_target_stays_a_path() {
    let alias = alias("alias Short = VeryLongTypeName;");
    assert_eq!(alias.ident.as_ref(), "Short");
    assert_eq!(alias.visibility, Visibility::Hidden);
    assert!(alias.generics.is_empty());
    let AliasTarget::Path(path) = &alias.target else {
        panic!("expected a path target");
    };
    assert_eq!(path.head.as_ref(), "VeryLongTypeName");
    assert!(path.tail.is_empty());
}

#[test]
fn a_qualified_target_keeps_every_segment() {
    let alias = alias("exposed alias S = std::string::String;");
    assert_eq!(alias.visibility, Visibility::Exposed);
    let AliasTarget::Path(path) = &alias.target else {
        panic!("expected a path target");
    };
    assert_eq!(path.head.as_ref(), "std");
    assert_eq!(
        path.tail.iter().map(|i| i.as_ref()).collect::<Vec<_>>(),
        ["string", "String"]
    );
}

#[test]
fn structural_targets_keep_their_written_type_syntax() {
    assert!(matches!(
        alias("alias Bytes = *[]u8;").target,
        AliasTarget::Type(Type::Pointer(_, false))
    ));
    assert!(matches!(
        alias("alias Buf = [16]u8;").target,
        AliasTarget::Type(Type::SizedArray(_, _))
    ));
    assert!(matches!(
        alias("alias Handler = (x: i32) => void;").target,
        AliasTarget::Type(Type::Function(_))
    ));
    assert!(matches!(
        alias("alias Specific = Map<*str, i32>;").target,
        AliasTarget::Type(Type::Generic(_, _))
    ));
}

#[test]
fn a_spec_conjunction_target_stays_structural_in_both_forms() {
    let AliasTarget::Type(Type::SpecStatic(members)) = alias("alias AB = spec A + B;").target
    else {
        panic!("expected a static spec conjunction target");
    };
    assert_eq!(members.len(), 2);

    let AliasTarget::Type(Type::Pointer(pointee, false)) = alias("alias Dyn = *spec B + A;").target
    else {
        panic!("expected a pointer target");
    };
    assert!(matches!(*pointee, Type::SpecStatic(_)));
}

#[test]
fn an_alias_may_own_generic_parameters_with_bounds_and_defaults() {
    let alias = alias("alias Keyed<V: Show = i32> = Map<*str, V>;");
    assert_eq!(alias.generics.len(), 1);
    assert_eq!(alias.generics[0].ident.as_ref(), "V");
    assert_eq!(alias.generics[0].bounds().len(), 1);
    assert!(alias.generics[0].default.is_some());
}

#[test]
fn an_expression_shaped_target_is_rejected() {
    for source in [
        "alias A = 1 + 2;",
        "alias A = f();",
        "alias A = \"text\";",
        "alias A = value.field;",
    ] {
        assert!(
            matches!(
                error_kinds(source).first(),
                Some(ParseErrorKind::Expected { .. })
            ),
            "{source} must be rejected as a non-type target"
        );
    }
}

#[test]
fn a_missing_equals_or_terminator_is_rejected() {
    assert!(matches!(
        error_kinds("alias A;").first(),
        Some(ParseErrorKind::Expected {
            expected: "'='",
            ..
        })
    ));
    assert!(matches!(
        error_kinds("alias A = B").first(),
        Some(ParseErrorKind::Expected {
            expected: "';'",
            ..
        })
    ));
}

#[test]
fn a_local_alias_is_rejected() {
    assert!(matches!(
        error_kinds("f() => void { alias A = i32; }").as_slice(),
        [ParseErrorKind::AliasNotAllowedHere]
    ));
}

#[test]
fn an_annotation_on_an_alias_is_rejected() {
    assert!(matches!(
        error_kinds("@symbol(name=\"x\") alias A = i32;").as_slice(),
        [ParseErrorKind::AnnotationNotAllowedHere]
    ));
}
