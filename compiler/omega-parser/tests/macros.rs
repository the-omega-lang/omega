use omega_parser::{
    SourceModule,
    lexer::{TokenKind, tokenize},
    macros,
    prelude::{Expression, Item, Statement},
};

fn expand(source: &str) -> SourceModule {
    macros::expand(SourceModule::parse(source).unwrap()).unwrap()
}

#[test]
fn expands_in_all_three_positions() {
    let module = expand(
        r#"
        macro make_items() => { one() => void {} two() => void {} }
        macro two_stmts() => { a := 1; b := 2; }
        macro sum($a: expr, $b: expr) => { $a + $b }
        make_items$();
        main() => i32 {
            two_stmts$();
            value := sum$(3, 4);
            sum$(3, 4);
            return value;
        }
    "#,
    );
    assert_eq!(module.nodes.len(), 3);
    let Item::FunctionDefinition(main) = &module.nodes[2].item else {
        panic!("expected main")
    };
    assert_eq!(main.codeblock.statements.len(), 5);
    assert!(matches!(
        main.codeblock.statements[0].statement,
        Statement::Walrus(_)
    ));
    assert!(matches!(
        main.codeblock.statements[1].statement,
        Statement::Walrus(_)
    ));
    assert!(matches!(
        main.codeblock.statements[2].statement,
        Statement::Walrus(_)
    ));
    assert!(matches!(
        main.codeblock.statements[3].statement,
        Statement::Expression(_)
    ));
    assert!(matches!(
        main.codeblock.statements[4].statement,
        Statement::Return(_)
    ));
}

#[test]
fn statement_invocation_inside_an_expression_is_not_spliced() {
    let module = expand(
        r#"
        macro sum($a: expr) => { $a }
        main() => i32 { x := sum$(1) + 2; return x; }
    "#,
    );
    let Item::FunctionDefinition(main) = &module.nodes[0].item else {
        panic!("expected main")
    };
    let Statement::Walrus(w) = &main.codeblock.statements[0].statement else {
        panic!("expected walrus")
    };
    assert!(matches!(w.value.expression, Expression::BinaryOp(_)));
}

#[test]
fn variadic_repetition_handles_empty_and_separators() {
    let module = expand(
        r#"
        macro calls($f: ident, $args: expr...) => { $...(){ $f($args); } }
        macro list($f: ident, $args: expr...) => { $f($...(,){ $args }) }
        sink(a: i32) => void {}
        pair(a: i32, b: i32) => i32 { a + b }
        main() => i32 {
            calls$(sink);
            calls$(sink, 1, 2);
            return list$(pair, 3, 4);
        }
    "#,
    );
    let Item::FunctionDefinition(main) = &module.nodes[2].item else {
        panic!("expected main")
    };
    assert_eq!(main.codeblock.statements.len(), 3);
}

#[test]
fn fragment_validation_and_definition_validation_are_precise() {
    let parsed = SourceModule::parse(
        r#"
        macro use_ident($name: ident) => { $name }
        main() => i32 { return use_ident$(3 + 4); }
    "#,
    )
    .unwrap();
    assert!(
        macros::expand(parsed)
            .unwrap_err()
            .to_string()
            .contains("does not parse as Ident")
    );

    for (source, expected) in [
        (
            "macro m($a: expr..., $b: expr) => {}",
            "variadic macro parameter must be the last",
        ),
        ("macro m($a: expr...) => { $a }", "outside a repetition"),
        ("macro m() => { $...(){ x; } }", "has no variadic parameter"),
        (
            "macro m($a: expr...) => { $...(){ x; } }",
            "does not reference its variadic",
        ),
    ] {
        let text = match SourceModule::parse(source) {
            Ok(module) => macros::expand(module).unwrap_err().to_string(),
            Err(errors) => errors.first().map(ToString::to_string).unwrap_or_default(),
        };
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn lexes_new_dollar_forms_and_rejects_old_invocation_syntax() {
    let (tokens, errors) = tokenize("name$(x) $...(,) { $name } $");
    assert!(errors.is_empty());
    assert!(matches!(tokens[1].kind, TokenKind::Dollar));
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::Metavar(_)))
    );
    assert!(matches!(tokens[tokens.len() - 2].kind, TokenKind::Dollar));
    assert!(SourceModule::parse("macro m() => { 1 } m!(1);").is_err());
}

#[test]
fn negative_macro_cases_report_the_new_diagnostics() {
    for (source, expected) in [
        (
            "macro m($a: expr...) => { $...(){ $...(){ $a } } }",
            "macro repetitions can't nest",
        ),
        (
            "macro m($a: expr...) => { $...((){ $a } }",
            "separator must be a single non-bracket token",
        ),
        (
            "macro m() => { x := 1; } main() => i32 { return m$(); }",
            "does not expand to a valid expression here",
        ),
        (
            "macro m() => {} main() => void { defer m$(); }",
            "can expand to more than one statement",
        ),
        (
            "macro m() => expr {}",
            "expected '{', found identifier 'expr'",
        ),
    ] {
        let text = match SourceModule::parse(source) {
            Ok(module) => macros::expand(module).unwrap_err().to_string(),
            Err(errors) => errors.first().map(ToString::to_string).unwrap_or_default(),
        };
        assert!(text.contains(expected), "{text}");
    }

    let module = SourceModule::parse(
        r#"
        macro m($fixed: expr, $rest: expr...) => { $...(){ $rest } }
        main() => i32 { return m$(); }
    "#,
    )
    .unwrap();
    assert!(
        macros::expand(module)
            .unwrap_err()
            .to_string()
            .contains("expects at least 1 argument(s)")
    );
}
