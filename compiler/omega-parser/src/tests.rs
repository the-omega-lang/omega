use super::*;
use crate::ast::item::Item;
use crate::ast::r#type::Type;
use crate::diagnostics::ParseErrorKind;

fn type_head(ty: &Type) -> &str {
    match ty {
        Type::Named(path) | Type::Generic(path, _) => path.head.as_ref(),
        other => panic!("unexpected type {other:?}"),
    }
}

fn expects(source: &str, expected: &str) -> bool {
    errors(source).iter().any(
        |e| matches!(e, ParseErrorKind::Expected { expected: found, .. } if *found == expected),
    )
}

fn items(source: &str) -> Vec<Item> {
    let (tokens, _) = lexer::tokenize(source);
    let mut parser = parser::Parser::new(&tokens);
    parser::item::parse_source_module(&mut parser)
        .into_iter()
        .map(|node| node.item)
        .collect()
}

fn conformance(source: &str) -> crate::ast::item::ConformStmt {
    let module = SourceModule::parse(source).expect("a conformance declaration should parse");
    match &module.nodes.last().expect("at least one item").item {
        Item::Conform(c) => c.clone(),
        other => panic!("unexpected trailing item {other:?}"),
    }
}

fn errors(source: &str) -> Vec<ParseErrorKind> {
    SourceModule::parse(source)
        .err()
        .expect("expected this source to be rejected")
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn parses_first_class_gap_and_glue_items() {
    let module = SourceModule::parse(
        "gap Allocator { alloc(size: usize) => *mut u8; }\n\
         glue core::platform::Allocator { alloc(size: usize) => *mut u8 { <*mut u8>0 } }",
    )
    .expect("gap and glue declarations should parse");
    assert!(matches!(module.nodes[0].item, Item::Gap(_)));
    assert!(matches!(module.nodes[1].item, Item::Glue(_)));
}

#[test]
fn gap_rejects_a_body_and_self_parameter() {
    assert!(SourceModule::parse("gap Invalid { f(*self) => void { } }").is_err());
    assert!(matches!(
        errors("gap Invalid { f(x: i32) => void { } }").as_slice(),
        [ParseErrorKind::GapFunctionBody { .. }]
    ));
    assert!(matches!(
        errors("gap Invalid { f(*self) => void; }").as_slice(),
        [ParseErrorKind::GapFunctionSelf { .. }]
    ));
}

#[test]
fn gap_and_glue_reject_a_visibility_modifier() {
    for source in [
        "exposed gap Foo { f() => void; }",
        "shared gap Foo { f() => void; }",
        "exposed glue Foo { f() => void { } }",
        "shared glue Foo { f() => void { } }",
    ] {
        assert!(
            matches!(
                errors(source).as_slice(),
                [ParseErrorKind::GapOrGlueVisibility]
            ),
            "expected exactly one visibility rejection for `{source}`"
        );
    }
}

#[test]
fn gap_functions_carry_visibility_and_default_to_exposed() {
    let module = SourceModule::parse(
        "gap Capability {
             implicit() => void;
             exposed anyone() => void;
             shared package_wide() => void;
             hidden declaring_module() => void;
         }",
    )
    .expect("gap members should accept the ordinary visibility modifiers");
    let Item::Gap(gap) = &module.nodes[0].item else {
        panic!("expected a gap definition");
    };
    let visibilities: Vec<Visibility> = gap.functions.iter().map(|f| f.visibility).collect();
    assert_eq!(
        visibilities,
        vec![
            Visibility::Exposed,
            Visibility::Exposed,
            Visibility::Shared,
            Visibility::Hidden,
        ]
    );
}

#[test]
fn gap_and_glue_reject_generics_without_cascading() {
    assert!(matches!(
        errors("gap Foo<T> { f() => void; }").as_slice(),
        [ParseErrorKind::GapOrGlueGeneric]
    ));
    assert!(matches!(
        errors("glue Foo<i32> { f() => void { } }").as_slice(),
        [ParseErrorKind::GapOrGlueGeneric]
    ));
    assert!(matches!(
        errors("gap Foo { f<T>() => void; }").as_slice(),
        [ParseErrorKind::GapOrGlueGeneric]
    ));
}

#[test]
fn parses_variadic_spec_functions_and_typed_for_in_bindings() {
    SourceModule::parse(
        "spec Log { write(*self, ...) => void; }\n\
         main() => void { for value : u8 in source { } }",
    )
    .expect("both new contextual grammar forms should parse");
}

#[test]
fn gap_and_glue_stay_ordinary_identifiers() {
    let module = SourceModule::parse(
        "gap := 5;\n\
         glue : i32 = 5;\n\
         gap Real { f() => void; }\n\
         glue Real { f() => void { } }\n\
         uses_them() => i32 { gap := 1; glue := 2; return gap + glue; }\n\
         glue() => i32 { return 0; }",
    )
    .expect("`gap`/`glue` must stay usable as ordinary identifiers");
    assert!(matches!(module.nodes[0].item, Item::Walrus(_)));
    assert!(matches!(
        module.nodes[1].item,
        Item::DeclarationWithInit(..)
    ));
    assert!(matches!(module.nodes[2].item, Item::Gap(_)));
    assert!(matches!(module.nodes[3].item, Item::Glue(_)));
    assert!(matches!(module.nodes[4].item, Item::FunctionDefinition(_)));
    assert!(matches!(module.nodes[5].item, Item::FunctionDefinition(_)));
}

#[test]
fn a_module_named_glue_coexists_with_a_glue_declaration() {
    let module = SourceModule::parse(
        "import glue::helper;\n\
         gap Real { f() => void; }\n\
         glue Real { f() => void { glue::helper(); } }",
    )
    .expect("a user module named `glue` must still parse");
    assert!(matches!(module.nodes[0].item, Item::Import(_)));
    assert!(matches!(module.nodes[2].item, Item::Glue(_)));
}

#[test]
fn parses_conform_and_primitive_items() {
    let module = SourceModule::parse(
        "spec Show { show(*self) => i32; }\n\
         struct Box<T> { value: T; }\n\
         meet<T> Show for Box<T> { show(*self) => i32 { 1 } }\n\
         primitive<T> []T { exposed is_empty(*self) => bool { self.length == 0 } }",
    )
    .expect("conform and primitive declarations should parse");
    assert!(matches!(module.nodes[2].item, Item::Conform(_)));
    assert!(matches!(module.nodes[3].item, Item::Primitive(_)));
}

#[test]
fn meet_maps_the_written_spec_and_target_to_their_semantic_fields() {
    let concrete = conformance(
        "spec Animal { speak(*self) => i32; }\n\
         struct Dog {}\n\
         meet Animal for Dog { speak(*self) => i32 { 1 } }",
    );
    assert_eq!(type_head(&concrete.spec), "Animal");
    assert_eq!(type_head(&concrete.target), "Dog");
    assert!(concrete.generics.is_empty());

    let blanket = conformance(
        "spec Animal { speak(*self) => i32; }\n\
         spec Tagged { tag(*self) => i32; }\n\
         meet<T: Animal> Tagged for T { tag(*self) => i32 { self.speak() } }",
    );
    assert_eq!(type_head(&blanket.spec), "Tagged");
    assert_eq!(type_head(&blanket.target), "T");
    assert_eq!(blanket.generics.len(), 1);
}

// The connector follows the spec so that names ending in a preposition-like
// word stay readable; this is the reason the declaration order was reversed.
#[test]
fn meet_accepts_connector_like_spec_names() {
    for (source, spec) in [
        (
            "spec ToIterator<T> { iter(*self) => T; }\n\
             meet ToIterator<char> for str { iter(*self) => char { 'a' } }",
            "ToIterator",
        ),
        (
            "spec WithCapacity { with_capacity(n: usize) => Self; }\n\
             struct Buf {}\n\
             meet WithCapacity for Buf { with_capacity(n: usize) => Self { Buf {} } }",
            "WithCapacity",
        ),
        (
            "spec AsBytes { as_bytes(*self) => []u8; }\n\
             struct Buf {}\n\
             meet AsBytes for Buf { as_bytes(*self) => []u8 { [] } }",
            "AsBytes",
        ),
    ] {
        let parsed = conformance(source);
        assert_eq!(type_head(&parsed.spec), spec);
    }
}

#[test]
fn conform_and_primitive_enforce_their_visibility_shapes() {
    assert!(matches!(
        errors("spec Show { show(*self) => i32; } struct S {} meet Show for S { exposed show(*self) => i32 { 1 } }").as_slice(),
        [ParseErrorKind::ConformMethodVisibility]
    ));
    assert!(matches!(
        errors("exposed primitive i32 { exposed value(*self) => i32 { *self } }").as_slice(),
        [ParseErrorKind::PrimitiveVisibility]
    ));
}

#[test]
fn meet_and_primitive_stay_contextual_identifiers() {
    let module = SourceModule::parse(
        "meet := 1;\n\
         primitive : i32 = 2;\n\
         meet() => i32 { primitive := 3; return primitive; }\n\
         primitive() => i32 { return meet; }",
    )
    .expect("meet and primitive must remain usable as ordinary identifiers");
    assert!(matches!(module.nodes[0].item, Item::Walrus(_)));
    assert!(matches!(
        module.nodes[1].item,
        Item::DeclarationWithInit(..)
    ));
    assert!(matches!(module.nodes[2].item, Item::FunctionDefinition(_)));
    assert!(matches!(module.nodes[3].item, Item::FunctionDefinition(_)));
}

#[test]
fn rejects_removed_conformance_syntax() {
    assert!(SourceModule::parse("spec Ops for i32 { value(*self) => i32; }").is_err());
    assert!(SourceModule::parse("spec Ops { value(*self) => i32; } struct S : Ops {}").is_err());
}

const SPEC_AND_STRUCT: &str = "spec Show { show(*self) => i32; } struct S {} ";

#[test]
fn a_started_spec_position_commits_to_the_conformance_grammar() {
    for declaration in ["meet Show S { }", "meet<T> Show T { }"] {
        let source = format!("{SPEC_AND_STRUCT}{declaration}");
        assert!(
            items(&source)
                .iter()
                .any(|item| matches!(item, Item::Conform(_))),
            "`{declaration}` should recover as a conformance missing its connector"
        );
        assert!(
            expects(&source, "'for'"),
            "`{declaration}` should report the missing `for` connector"
        );
    }

    for (declaration, expected) in [
        ("meet Show : S { }", "'for'"),
        ("meet Show for { }", "a type"),
        ("meet<T> Show for T", "'{'"),
    ] {
        let source = format!("{SPEC_AND_STRUCT}{declaration}");
        assert!(
            expects(&source, expected),
            "`{declaration}` should report a missing {expected}, got {:?}",
            errors(&source)
        );
    }
}

#[test]
fn a_shape_that_cannot_start_a_spec_leaves_meet_an_identifier() {
    for source in [
        "meet []u8 for S { }",
        "meet *S for Show { }",
        "meet spec A for S { }",
        "meet for S { }",
    ] {
        assert!(
            !items(source)
                .iter()
                .any(|item| matches!(item, Item::Conform(_))),
            "`{source}` cannot be a conformance and must not be parsed as one"
        );
    }
}
#[test]
fn chained_comparison_reports_its_own_error() {
    assert!(matches!(
        errors("a := b < c < d;").as_slice(),
        [ParseErrorKind::ChainedComparison]
    ));
}

#[test]
fn glue_rejects_generic_and_self_taking_functions() {
    for source in [
        "glue Foo { f<T>(x: T) => void { } }",
        "glue Foo { f(*self) => void { } }",
    ] {
        assert!(
            errors(source)
                .iter()
                .any(|e| matches!(e, ParseErrorKind::GlueFunctionShape { .. })),
            "`{source}` should report GlueFunctionShape, got {:?}",
            errors(source)
        );
    }
}

fn recovered_members(source: &str) -> (Vec<String>, usize) {
    let (tokens, lex_errors) = lexer::tokenize(source);
    let mut parser = parser::Parser::new(&tokens);
    let nodes = parser::item::parse_source_module(&mut parser);
    let error_count = lex_errors.len() + parser.into_errors().len();
    let names: Vec<Ident> = match &nodes.last().expect("at least one item").item {
        Item::Conform(c) => c.functions.iter().map(|f| f.ident.clone()).collect(),
        Item::Primitive(pr) => pr.functions.iter().map(|f| f.ident.clone()).collect(),
        Item::Gap(g) => g.functions.iter().map(|f| f.ident.clone()).collect(),
        Item::Glue(g) => g.functions.iter().map(|f| f.ident.clone()).collect(),
        Item::Struct(s) => s.functions.iter().map(|f| f.ident.clone()).collect(),
        other => panic!("unexpected trailing item {other:?}"),
    };
    (
        names.into_iter().map(|i| i.as_ref().to_string()).collect(),
        error_count,
    )
}

#[test]
fn every_item_body_recovers_per_member() {
    for source in [
        "struct S { bad(*self) => ? { } good(*self) => i32 { 1 } }",
        "spec Sp { m(*self) => i32; }\n\
         struct S {}\n\
         meet Sp for S { bad(*self) => ? { } good(*self) => i32 { 1 } }",
        "primitive i32 { bad(*self) => ? { } good(*self) => i32 { 1 } }",
        "gap G { bad() => ?; good() => i32; }",
        "glue G { bad() => ? { } good() => i32 { 1 } }",
    ] {
        let (members, error_count) = recovered_members(source);
        assert_eq!(
            members,
            ["good"],
            "`{source}` should keep parsing after the bad member"
        );
        assert_eq!(
            error_count, 1,
            "`{source}` should report exactly one error, not cascade"
        );
    }
}

#[test]
fn every_parse_error_renders_a_headline_and_a_label() {
    use crate::ast::identifier::Ident;
    use crate::diagnostics::{ParseError, Span};

    let name = || Ident("f".to_string());
    let kinds = [
        ParseErrorKind::Expected {
            expected: "a type",
            found: "';'".to_string(),
        },
        ParseErrorKind::UnterminatedString,
        ParseErrorKind::UnterminatedChar,
        ParseErrorKind::UnterminatedComment,
        ParseErrorKind::EvenMultilineStringDelimiter { count: 4 },
        ParseErrorKind::UnterminatedGroup { open: '(' },
        ParseErrorKind::UnterminatedGroup { open: '[' },
        ParseErrorKind::UnterminatedGroup { open: '{' },
        ParseErrorKind::InvalidCharacter('\u{7}'),
        ParseErrorKind::InvalidUnicodeEscape("D800".to_string()),
        ParseErrorKind::InvalidCharLiteral,
        ParseErrorKind::StructLiteralNotAllowedHere,
        ParseErrorKind::EnumFunctionBeforeSemi,
        ParseErrorKind::EnumNotAllowedHere,
        ParseErrorKind::StructNotAllowedHere,
        ParseErrorKind::UnionNotAllowedHere,
        ParseErrorKind::SpecNotAllowedHere,
        ParseErrorKind::RangeMissingEnd,
        ParseErrorKind::OpenRangeHasEnd,
        ParseErrorKind::ChainedComparison,
        ParseErrorKind::NestingTooDeep { limit: 64 },
        ParseErrorKind::AnnotationNotAllowedHere,
        ParseErrorKind::VisibilityNotAllowedHere,
        ParseErrorKind::GapOrGlueVisibility,
        ParseErrorKind::ConformMethodVisibility,
        ParseErrorKind::PrimitiveVisibility,
        ParseErrorKind::GapOrGlueGeneric,
        ParseErrorKind::GapFunctionBody { name: name() },
        ParseErrorKind::GapFunctionSelf { name: name() },
        ParseErrorKind::GlueFunctionShape { name: name() },
        ParseErrorKind::DefaultGenericParamNotTrailing { name: name() },
        ParseErrorKind::SpecDependenciesRemoved,
        ParseErrorKind::MacroInvocationNotAllowedAfterDefer,
        ParseErrorKind::VariadicMacroParamNotLast,
        ParseErrorKind::InvalidMacroSeparator,
        ParseErrorKind::NestedMacroRepetition,
        ParseErrorKind::ImportInMacroBody,
    ];

    for kind in kinds {
        let rendered = ParseError::new(Span::new(0, 1), kind.clone()).to_diagnostic();
        assert!(
            !rendered.message.trim().is_empty(),
            "{kind:?} renders no headline"
        );
        assert!(!rendered.labels.is_empty(), "{kind:?} renders no label");
        assert_eq!(kind.to_string(), rendered.message, "{kind:?}");
    }
}

#[test]
fn independent_syntax_errors_in_one_module_are_all_reported() {
    let source = "\
first() => i32 { 1 + ; }

second() => i32 { 2 }

third() => i32 { 3 * ; }
";
    let errors = errors(source);
    assert!(
        errors.len() >= 2,
        "each malformed function must report its own error: {errors:?}"
    );
    let items = items(source);
    assert_eq!(
        items.len(),
        3,
        "recovery must keep the well-formed neighbour: {items:?}"
    );
}

#[test]
fn a_malformed_member_does_not_swallow_its_enclosing_brace() {
    let source = "\
struct Holder {
    good: i32;
    bad: ;
}

after() => i32 { 0 }
";
    assert!(!errors(source).is_empty(), "the bad field must be reported");
    let items = items(source);
    assert_eq!(
        items.len(),
        2,
        "the item after the struct must still parse: {items:?}"
    );
    assert!(matches!(items[1], Item::FunctionDefinition(_)), "{items:?}");
}

#[test]
fn identifier_heavy_malformed_input_still_terminates() {
    // Nothing here commits to an item, so every resynchronization point is an
    // identifier. Reaching the assertions at all is the property under test:
    // recovery consumed input instead of stalling on the same token.
    let source = "alpha beta gamma\n>>> delta epsilon\n";
    assert!(!errors(source).is_empty());
    assert!(
        items(source).len() < 5,
        "recovery must not manufacture an item per identifier: {:?}",
        items(source)
    );
}

#[test]
fn a_stray_closing_brace_at_module_level_is_discarded_once() {
    let source = "}\n\nafter() => i32 { 0 }\n";
    assert!(!errors(source).is_empty());
    let items = items(source);
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(matches!(items[0], Item::FunctionDefinition(_)), "{items:?}");
}
