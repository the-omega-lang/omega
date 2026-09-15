use omega_parser::SourceModule;
use omega_parser::prelude::*;

fn parse(source: &str) -> SourceModule {
    SourceModule::parse(source).unwrap_or_else(|errors| panic!("{source:?} must parse: {errors:?}"))
}

fn errors(source: &str) -> Vec<ParseError> {
    SourceModule::parse(source).expect_err("this source must be rejected")
}

fn condition(source: &str) -> AnnotationExpr {
    let module = parse(source);
    let [node] = module.nodes.as_slice() else {
        panic!("expected exactly one item in {source:?}");
    };
    let [annotation] = node.conditions.as_slice() else {
        panic!("expected exactly one condition in {source:?}");
    };
    assert_eq!(annotation.name.as_ref(), "cond");
    let [AnnotationArg::Positional(expr)] = annotation.args.as_slice() else {
        panic!("expected one positional argument in {source:?}");
    };
    expr.clone()
}

fn on_global(condition: &str) -> String {
    format!("@cond({condition})\nexposed a : i32 = 1;\n")
}

#[test]
fn a_condition_is_kept_apart_from_the_items_other_annotations() {
    let module = parse("@cond(true)\n@symbol(export)\nexposed a : i32 = 1;\n");
    let [node] = module.nodes.as_slice() else {
        panic!("expected one item");
    };
    assert_eq!(node.conditions.len(), 1);
    let Item::DeclarationWithInit { annotations, .. } = &node.item else {
        panic!("expected a global binding, got {:?}", node.item);
    };
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].name.as_ref(), "symbol");
}

#[test]
fn every_literal_kind_parses_as_a_condition_operand() {
    for (source, expected) in [
        ("true", "a boolean"),
        ("equals(1, 1)", "a call"),
        ("equals(-1, 1)", "a call"),
        ("equals(1.5f64, 1.5)", "a call"),
        ("equals('x', 'x')", "a call"),
        ("equals(\"a\", \"a\")", "a call"),
        ("equals(b\"a\", b\"a\")", "a call"),
        ("equals(0xff_u8, 255)", "a call"),
        ("def::flag", "a qualified reference"),
        ("target_freestanding", "a name"),
    ] {
        assert_eq!(
            condition(&on_global(source)).describe(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn calls_and_lists_nest_to_any_depth() {
    let expr = condition(&on_global(
        "all(not(def::a), any(def::b, in(target_arch, &[\"x86_64\", \"aarch64\"])))",
    ));
    let AnnotationExprKind::Call { name, args } = &expr.kind else {
        panic!("expected a call");
    };
    assert_eq!(name.as_ref(), "all");
    assert_eq!(args.len(), 2);
    let AnnotationExprKind::Call { args: inner, .. } = &args[1].kind else {
        panic!("expected a nested call");
    };
    let AnnotationExprKind::Call {
        args: membership, ..
    } = &inner[1].kind
    else {
        panic!("expected a nested call");
    };
    let AnnotationExprKind::List(elements) = &membership[1].kind else {
        panic!("expected a list");
    };
    assert_eq!(elements.len(), 2);
}

#[test]
fn a_qualified_reference_has_exactly_two_segments() {
    let expr = condition(&on_global("def::flag"));
    let AnnotationExprKind::Qualified { namespace, name } = &expr.kind else {
        panic!("expected a qualified reference");
    };
    assert_eq!((namespace.as_ref(), name.as_ref()), ("def", "flag"));

    assert!(!errors(&on_global("def::a::b")).is_empty());
    assert!(!errors(&on_global("def::")).is_empty());
}

#[test]
fn a_nested_list_or_call_may_end_with_a_trailing_comma() {
    for source in [
        "all(\n\tdef::a,\n\tdef::b,\n)",
        "in(target_os, &[\n\t\"linux\",\n])",
        "all()",
        "in(target_os, &[])",
    ] {
        condition(&on_global(source));
    }
}

#[test]
fn a_missing_separator_is_still_an_error() {
    for source in [
        "all(def::a def::b)",
        "in(target_os, &[\"linux\" \"macos\"])",
        "all(def::a,, def::b)",
    ] {
        assert!(
            !errors(&on_global(source)).is_empty(),
            "{source} must be rejected"
        );
    }
}

#[test]
fn an_argument_that_is_not_an_annotation_expression_is_rejected() {
    for source in [
        "1 + 1",
        "(true)",
        "[1]",
        "&1",
        "def::flag()",
        "flag$()",
        "*flag",
    ] {
        assert!(
            !errors(&on_global(source)).is_empty(),
            "{source} must be rejected"
        );
    }
}

#[test]
fn nesting_beyond_the_parser_limit_is_diagnosed_rather_than_overflowing() {
    let depth = 2_000;
    let source = on_global(&format!(
        "{}true{}",
        "not(".repeat(depth),
        ")".repeat(depth)
    ));
    let reported = errors(&source);
    assert!(
        reported
            .iter()
            .any(|error| matches!(error.kind, ParseErrorKind::NestingTooDeep { .. })),
        "{reported:?}"
    );
}

#[test]
fn a_condition_parses_on_every_top_level_item_form() {
    let items = [
        "exposed a : i32 = 1;",
        "a := 1;",
        "comp a := 1;",
        "exposed f() => void { }",
        "struct S { exposed a: i32; }",
        "marker M {}",
        "enum E { A, B; }",
        "union U { exposed a: i32; }",
        "spec Sp { f() => i32; }",
        "gap G { f() => i32; }",
        "glue G { f() => i32 { return 1; } }",
        "meet Sp for i32 { }",
        "primitive i32 { }",
        "import core::option::Option;",
        "alias A = i32;",
        "macro m() => { }",
        "m$();",
        "foreign f : i32;",
        "foreign(c) g() => i32;",
        "foreign(c) { h() => i32; }",
    ];
    for item in items {
        let source = format!("@cond(def::flag)\n{item}\n");
        let module = parse(&source);
        assert_eq!(
            module.nodes.len(),
            1,
            "{item} must parse as exactly one item"
        );
        assert_eq!(
            module.nodes[0].conditions.len(),
            1,
            "{item} must keep its condition"
        );
    }
}

#[test]
fn a_condition_below_the_top_level_is_rejected_where_annotations_are_read() {
    let sources = [
        "struct S {\n\texposed a: i32;\n\n\t@cond(true)\n\tf(*self) => i32 { return 1; }\n}",
        "meet Sp for i32 {\n\t@cond(true)\n\tf() => i32 { return 1; }\n}",
        "primitive i32 {\n\t@cond(true)\n\tf(*self) => i32 { return 1; }\n}",
        "foreign(c) {\n\t@cond(true)\n\tf() => i32;\n}",
        // A generic declaration is never instantiated here, so only the
        // grammar can reject what it contains.
        "struct Holder<T> {\n\texposed value: T;\n\n\t@cond(true)\n\tget(*self) => T { return self.value; }\n}",
    ];
    for source in sources {
        let reported = errors(source);
        assert!(
            reported
                .iter()
                .any(|error| matches!(error.kind, ParseErrorKind::ConditionNotAllowedHere)),
            "{source} must be rejected as a misplaced condition, got {reported:?}"
        );
    }
}

/// Specs, enum variants, fields, and statements carry no annotations at all,
/// so a condition there is rejected by the surrounding grammar instead.
#[test]
fn a_condition_where_no_annotation_is_accepted_is_rejected_too() {
    for source in [
        "spec Sp {\n\t@cond(true)\n\tf() => i32;\n}",
        "enum E {\n\t@cond(true)\n\tA,\n\tB;\n}",
        "f() => void {\n\t@cond(true)\n\tx := 1;\n}",
    ] {
        assert!(!errors(source).is_empty(), "{source} must be rejected");
    }
}

#[test]
fn an_unattached_condition_is_diagnosed_as_an_annotation_without_an_item() {
    let reported = errors("@cond(true)\n");
    assert!(
        reported
            .iter()
            .any(|error| matches!(error.kind, ParseErrorKind::AnnotationWithoutItem)),
        "{reported:?}"
    );
}

/// The argument grammar is shared by every annotation; which forms mean
/// anything is each annotation's own business, checked after parsing.
#[test]
fn an_ordinary_annotation_accepts_the_same_argument_grammar() {
    let module = parse("@inline(equals(1, 1))\nexposed f() => void { }\n");
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("expected a function");
    };
    let [AnnotationArg::Positional(expr)] = f.annotations[0].args.as_slice() else {
        panic!("expected one positional argument");
    };
    assert!(matches!(expr.kind, AnnotationExprKind::Call { .. }));
}

#[test]
fn existing_annotation_spellings_still_parse_as_before() {
    let module = parse(concat!(
        "@layout(pack = sizeof<usize>, align = 8)\n",
        "struct S { exposed a: i32; }\n",
        "@symbol(mangle = disabled, export)\n",
        "exposed g : i32 = 1;\n",
        "@suppress(unused_import, dead_code)\n",
        "import core::option::Option;\n",
    ));
    let Item::Struct(s) = &module.nodes[0].item else {
        panic!("expected a struct");
    };
    let [
        AnnotationArg::KeyValue(pack, pack_value),
        AnnotationArg::KeyValue(align, align_value),
    ] = s.annotations[0].args.as_slice()
    else {
        panic!("expected two key = value arguments");
    };
    assert_eq!((pack.as_ref(), align.as_ref()), ("pack", "align"));
    assert!(matches!(pack_value.kind, AnnotationExprKind::Sizeof(_)));
    assert!(matches!(
        align_value.kind,
        AnnotationExprKind::Literal(AnnotationLiteral::Number { .. })
    ));

    let Item::Import(import) = &module.nodes[2].item else {
        panic!("expected an import");
    };
    let names: Vec<&str> = import.annotations[0]
        .args
        .iter()
        .map(|arg| arg.bare_name().expect("a bare warning name").as_ref())
        .collect();
    assert_eq!(names, ["unused_import", "dead_code"]);
}

#[test]
fn a_standalone_literal_decodes_without_a_surrounding_module() {
    assert!(matches!(
        parse_literal("true"),
        Ok(AnnotationLiteral::Bool(true))
    ));
    assert!(matches!(
        parse_literal("-1_000i64"),
        Ok(AnnotationLiteral::Number { negative: true, .. })
    ));
    assert!(matches!(
        parse_literal("'\\n'"),
        Ok(AnnotationLiteral::Char('\n'))
    ));
    assert!(matches!(
        parse_literal("b\"x\""),
        Ok(AnnotationLiteral::ByteStr(_))
    ));

    for invalid in ["", "name", "1 2", "1 + 1", "\"a\" \"b\"", "&[1]", "-", "-x"] {
        assert!(
            parse_literal(invalid).is_err(),
            "{invalid:?} is not one literal"
        );
    }
}
