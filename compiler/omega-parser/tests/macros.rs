use omega_parser::{
    SourceModule,
    lexer::{TokenKind, tokenize},
    macros,
    prelude::{Expression, Item, Statement},
};
use std::collections::HashMap;

fn expand(source: &str) -> SourceModule {
    macros::expand(SourceModule::parse(source).unwrap(), &HashMap::new()).unwrap()
}

fn macro_definition(source: &str) -> omega_parser::prelude::MacroDefinitionStmt {
    let module = SourceModule::parse(source).unwrap();
    let Item::MacroDefinition(definition) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected macro definition");
    };
    definition
}

#[test]
fn imported_macro_definitions_merge_and_local_definitions_shadow() {
    let imported = macro_definition("macro make() => { imported() => void {} }");
    let mut definitions = HashMap::new();
    definitions.insert(imported.name.clone(), imported);

    let module = macros::expand(SourceModule::parse("make$();").unwrap(), &definitions).unwrap();
    assert_eq!(module.nodes.len(), 1);

    let module = macros::expand(
        SourceModule::parse("macro make() => { local() => void {} } make$();").unwrap(),
        &definitions,
    )
    .unwrap();
    let Item::FunctionDefinition(function) = &module.nodes[0].item else {
        panic!("expected expanded function");
    };
    assert_eq!(function.ident.0, "local");
}

#[test]
fn imported_macro_expansions_are_attributed_to_the_call_site() {
    let imported = macro_definition("exposed macro imported_macro() => { side_effect(); }");
    let mut definitions = HashMap::new();
    definitions.insert(imported.name.clone(), imported);

    let source = "main() => void { imported_macro$(); }";
    let parsed = SourceModule::parse(source).unwrap();
    let Item::FunctionDefinition(function) = &parsed.nodes[0].item else {
        panic!("expected main function");
    };
    let call_span = function.codeblock.statements[0].span;

    let expanded = macros::expand(parsed, &definitions).unwrap();
    let Item::FunctionDefinition(function) = &expanded.nodes[0].item else {
        panic!("expected expanded main function");
    };
    let statement = &function.codeblock.statements[0];
    assert_eq!(statement.span, call_span);
    let Statement::Expression(expression) = &statement.statement else {
        panic!("expected foreign macro body to become an expression statement");
    };
    assert_eq!(expression.span, call_span);
}

#[test]
fn macro_visibility_and_definition_expansion_are_reported() {
    use omega_parser::prelude::Visibility;

    let exposed = macro_definition("exposed macro make() => {}");
    let shared = macro_definition("shared macro make() => {}");
    assert_eq!(exposed.visibility, Visibility::Exposed);
    assert_eq!(shared.visibility, Visibility::Shared);
    assert!(SourceModule::parse("exposed make$();").is_err());

    let error = macros::expand(
        SourceModule::parse("macro outer() => { macro inner() => {} } outer$();").unwrap(),
        &HashMap::new(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        macros::MacroError::MacroDefinitionInExpansion { macro_name } if macro_name.0 == "inner"
    ));
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
fn asm_reg_expr_macro_expands_but_body_stays_opaque() {
    let module = expand(
        r#"
        macro wrap($v: expr) => { f() => void { asm(reg($v)) => { mov $v, 1 } } }
        wrap$(42);
    "#,
    );
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("expected expanded function");
    };
    let Statement::InlineAsm(asm) = &f.codeblock.statements[0].statement else {
        panic!("expected an inline-asm statement");
    };
    let omega_parser::prelude::AsmDescriptorKind::Reg { expr, .. } = &asm.descriptors[0].kind
    else {
        panic!("expected a reg descriptor");
    };
    // The macro parameter substituted into the `reg(...)` expression...
    assert!(matches!(expr.expression, Expression::Number(_)));
    // ...but the literal `$v` inside the raw body is not macro syntax at
    // all -- it survives untouched as asm-operand-binding text.
    assert_eq!(asm.body.trim(), "mov $v, 1");
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
        macros::expand(parsed, &HashMap::new())
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
            Ok(module) => macros::expand(module, &HashMap::new())
                .unwrap_err()
                .to_string(),
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
            Ok(module) => macros::expand(module, &HashMap::new())
                .unwrap_err()
                .to_string(),
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
        macros::expand(module, &HashMap::new())
            .unwrap_err()
            .to_string()
            .contains("expects at least 1 argument(s)")
    );
}

#[test]
fn path_fragment_accepts_ordinary_path_syntax_and_reparses_as_a_path() {
    let module = expand(
        r#"
        macro re_export($path: path, $ident: ident) => { alias $ident = $path; }
        struct Holder { x: i32; }
        re_export$(root::a::b::Holder, Reexported);
    "#,
    );
    let Item::Alias(alias) = &module.nodes[1].item else {
        panic!("expected expanded alias item");
    };
    assert_eq!(alias.ident.0, "Reexported");
    let omega_parser::prelude::AliasTarget::Path(path) = &alias.target else {
        panic!("expected the substituted `$path` to reparse as a path alias target");
    };
    assert!(matches!(
        path.anchor,
        Some(omega_parser::prelude::PathAnchor::Root)
    ));
    assert_eq!(path.head.0, "a");
    assert_eq!(path.tail.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(), [
        "b", "Holder"
    ]);
}

#[test]
fn path_fragment_accepts_every_path_anchor_form() {
    for (arg, expected_anchor) in [
        ("plain::segment", None),
        ("root::a", Some("root")),
        ("self::a", Some("self")),
        ("super::super::a", Some("super")),
    ] {
        let source = format!(
            r#"
            macro id($p: path) => {{ alias Out = $p; }}
            id$({arg});
        "#
        );
        let module = expand(&source);
        let Item::Alias(alias) = &module.nodes[0].item else {
            panic!("expected expanded alias item for `{arg}`");
        };
        let omega_parser::prelude::AliasTarget::Path(path) = &alias.target else {
            panic!("expected a path alias target for `{arg}`");
        };
        match expected_anchor {
            Some("root") => assert!(matches!(
                path.anchor,
                Some(omega_parser::prelude::PathAnchor::Root)
            )),
            Some("self") => assert!(matches!(
                path.anchor,
                Some(omega_parser::prelude::PathAnchor::SelfModule)
            )),
            Some("super") => assert!(matches!(
                path.anchor,
                Some(omega_parser::prelude::PathAnchor::Super(2))
            )),
            _ => assert!(path.anchor.is_none(), "`{arg}` should be unanchored"),
        }
    }
}

#[test]
fn path_fragment_rejects_non_path_trailing_syntax() {
    let parsed = SourceModule::parse(
        r#"
        macro id($p: path) => { alias Out = $p; }
        main() => void { id$(a::b + 1); }
    "#,
    )
    .unwrap();
    let text = macros::expand(parsed, &HashMap::new())
        .unwrap_err()
        .to_string();
    assert!(text.contains("does not parse as Path"), "{text}");
}

#[test]
fn path_fragment_supports_variadic_repetition() {
    let module = expand(
        r#"
        macro touch($paths: path...) => { $...(){ $paths; } }
        main() => void { touch$(a::b, root::c::d); }
    "#,
    );
    let Item::FunctionDefinition(main) = &module.nodes[0].item else {
        panic!("expected main")
    };
    assert_eq!(main.codeblock.statements.len(), 2);
    for statement in &main.codeblock.statements {
        assert!(matches!(statement.statement, Statement::Expression(_)));
    }
}

#[test]
fn import_in_a_macro_body_is_rejected_at_definition_time() {
    let errors = SourceModule::parse("macro m() => { import io::Write; }").unwrap_err();
    assert!(errors.iter().any(|error| {
        error
            .to_string()
            .contains("imports are not allowed in macro bodies")
    }));
}
