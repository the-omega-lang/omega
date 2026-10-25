use omega_parser::SourceModule;
use omega_parser::diagnostics::{ParseError, ParseErrorKind};
use omega_parser::parser::contextual;
use omega_parser::prelude::Item;

#[test]
fn every_contextual_keyword_stays_an_ordinary_identifier() {
    for word in contextual::ALL {
        let source = format!(
            "{word} := 1;\n\
             use_it() => i32 {{ {word} := 2; return {word}; }}\n\
             {word}_fn() => i32 {{ return 0; }}"
        );
        let module = SourceModule::parse(&source)
            .unwrap_or_else(|e| panic!("`{word}` must stay usable as an identifier: {e:?}"));
        assert!(
            matches!(module.nodes[0].item, Item::Walrus { .. }),
            "`{word}` should have parsed as a top-level binding"
        );
        assert_eq!(module.nodes.len(), 3, "`{word}`: expected three items");
    }
}

#[test]
fn every_contextual_keyword_works_as_a_parameter_and_field_name() {
    for word in contextual::ALL {
        let params = if *word == contextual::SELF {
            format!("a: i32, {word}: i32")
        } else {
            format!("{word}: i32")
        };
        let source = format!(
            "struct Holder {{ {word}: i32; }}\n\
             takes({params}) => i32 {{ return 0; }}"
        );
        SourceModule::parse(&source)
            .unwrap_or_else(|e| panic!("`{word}` must work as a parameter and field name: {e:?}"));
    }
}

#[test]
fn reserved_keywords_are_not_identifiers() {
    for word in ["comp", "reveal", "match", "loop", "true"] {
        for source in [
            format!("{word} := 1;"),
            format!("use_it() => void {{ {word} := 2; }}"),
            format!("use_it() => void {{ ^{word} : i32 = 2; }}"),
            format!("struct Holder {{ {word}: i32; }}"),
            format!("struct {word} {{ }}"),
            format!("takes({word}: i32) => void {{ }}"),
            format!("takes(a: i32, {word}: i32) => void {{ }}"),
        ] {
            let errors = SourceModule::parse(&source)
                .err()
                .unwrap_or_else(|| panic!("`{word}` is reserved, so `{source}` must not parse"));
            assert!(
                matches!(
                    errors.as_slice(),
                    [ParseError { kind: ParseErrorKind::KeywordAsName { keyword }, .. }]
                        if *keyword == word
                ),
                "`{source}` should report one keyword used as a name, got {errors:?}"
            );
        }
    }
}

#[test]
fn the_registry_has_no_duplicates() {
    let mut sorted = contextual::ALL.to_vec();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate entry in contextual::ALL");
}

/// The type queries commit only when `<Type>` follows, which is what lets
/// each stay an ordinary identifier above.
#[test]
fn the_type_query_forms_parse_as_expressions() {
    for word in [
        contextual::SIZEOF,
        contextual::ALIGNOF,
        contextual::TYPEINFO,
    ] {
        let source = format!("query() => usize {{ {word}<[4]u8> }}");
        SourceModule::parse(&source)
            .unwrap_or_else(|e| panic!("`{word}<Type>` must parse as an expression: {e:?}"));
    }
}

#[test]
fn typeinfo_before_a_type_is_the_query_and_elsewhere_a_name() {
    let module = SourceModule::parse("query() => void { a := typeinfo<i32>; b := typeinfo; }")
        .expect("both forms must parse");
    let rendered = format!("{:?}", module.nodes);
    assert!(rendered.contains("Typeinfo(TypeinfoExpr"), "{rendered}");
    assert_eq!(rendered.matches("Typeinfo(").count(), 1, "{rendered}");
}
