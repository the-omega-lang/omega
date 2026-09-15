use super::*;
use omega_parser::prelude::NumberExpr;

fn annotation(args: Vec<HirAnnotationArg>) -> HirAnnotation {
    HirAnnotation {
        name: Ident("symbol".into()),
        args,
        span: Span::new(0, 0),
    }
}

fn number(text: &str) -> HirAnnotationValue {
    expr(AnnotationExprKind::Literal(AnnotationLiteral::Number {
        negative: false,
        value: NumberExpr {
            base: NumberBase::Decimal,
            integer_part: text.into(),
            fractional_part: None,
            explicit_type: None,
        },
    }))
}

fn expr(kind: AnnotationExprKind) -> HirAnnotationValue {
    HirAnnotationValue {
        kind,
        span: Span::new(0, 0),
        origin: Default::default(),
    }
}

fn ident_arg(name: &str) -> HirAnnotationArg {
    HirAnnotationArg::Positional(expr(AnnotationExprKind::Name(Ident(name.into()))))
}

fn key_ident(key: &str, value: &str) -> HirAnnotationArg {
    HirAnnotationArg::KeyValue(
        Ident(key.into()),
        expr(AnnotationExprKind::Name(Ident(value.into()))),
    )
}

fn key_str(key: &str, value: &str) -> HirAnnotationArg {
    HirAnnotationArg::KeyValue(
        Ident(key.into()),
        expr(AnnotationExprKind::Literal(AnnotationLiteral::Str(
            value.into(),
        ))),
    )
}

fn resolve(args: Vec<HirAnnotationArg>) -> Result<SymbolPolicy, String> {
    resolve_symbol(&annotation(args), &SymbolPolicy::ordinary())
}

#[test]
fn mangle_selects_the_naming_mode() {
    assert_eq!(
        resolve(vec![key_ident("mangle", "enabled")]),
        Ok(SymbolPolicy::ordinary())
    );
    assert_eq!(
        resolve(vec![key_ident("mangle", "disabled")]),
        Ok(SymbolPolicy::mangled(ManglingMode::Disabled))
    );
}

#[test]
fn name_selects_an_exact_symbol() {
    assert_eq!(
        resolve(vec![key_str("name", "my_symbol")]),
        Ok(SymbolPolicy::mangled(ManglingMode::Forced(
            "my_symbol".into()
        )))
    );
    assert!(resolve(vec![key_str("name", "")]).is_err());
    assert!(resolve(vec![key_str("name", "with\0nul")]).is_err());
    assert!(resolve(vec![key_ident("name", "enabled")]).is_err());
}

#[test]
fn an_omitted_parameter_keeps_the_item_default() {
    let foreign = resolve_symbol(
        &annotation(vec![key_ident("mangle", "enabled")]),
        &SymbolPolicy::foreign(),
    );
    assert_eq!(
        foreign,
        Ok(SymbolPolicy {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Default,
        }),
        "a foreign item that opts into mangling keeps its cross-image visibility"
    );
    assert_eq!(
        resolve_symbol(
            &annotation(vec![key_ident("export", "disabled")]),
            &SymbolPolicy::foreign(),
        ),
        Ok(SymbolPolicy::mangled(ManglingMode::Disabled)),
        "and can still opt back into a symbol resolved inside this image"
    );
    assert_eq!(
        resolve(vec![key_ident("export", "disabled")]),
        Ok(SymbolPolicy::ordinary())
    );
}

#[test]
fn export_combines_with_either_naming_parameter() {
    assert_eq!(
        resolve(vec![key_str("name", "exact"), ident_arg("export")]),
        Ok(SymbolPolicy {
            mangling: ManglingMode::Forced("exact".into()),
            visibility: SymbolVisibility::Default,
        })
    );
    assert_eq!(
        resolve(vec![
            key_ident("export", "enabled"),
            key_ident("mangle", "disabled")
        ]),
        Ok(SymbolPolicy {
            mangling: ManglingMode::Disabled,
            visibility: SymbolVisibility::Default,
        })
    );
}

#[test]
fn name_and_mangle_conflict_in_either_order() {
    assert!(
        resolve(vec![
            key_str("name", "exact"),
            key_ident("mangle", "enabled")
        ])
        .is_err()
    );
    assert!(
        resolve(vec![
            key_ident("mangle", "disabled"),
            key_str("name", "exact")
        ])
        .is_err()
    );
}

#[test]
fn only_export_may_be_written_bare() {
    assert_eq!(
        resolve(vec![ident_arg("export")]),
        Ok(SymbolPolicy {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Default,
        })
    );
    assert!(resolve(vec![ident_arg("mangle")]).is_err());
    assert!(resolve(vec![ident_arg("enabled")]).is_err());
    assert!(resolve(vec![ident_arg("name")]).is_err());
}

#[test]
fn malformed_arguments_are_rejected() {
    assert!(resolve(vec![]).is_err());
    assert!(resolve(vec![key_ident("mangle", "sometimes")]).is_err());
    assert!(resolve(vec![key_ident("export", "sometimes")]).is_err());
    assert!(resolve(vec![key_ident("nonsense", "enabled")]).is_err());
    assert!(
        resolve(vec![HirAnnotationArg::KeyValue(
            Ident("mangle".into()),
            number("3")
        )])
        .is_err()
    );
}

#[test]
fn a_repeated_key_is_rejected_however_it_is_spelled() {
    assert!(resolve(vec![ident_arg("export"), key_ident("export", "enabled")]).is_err());
    assert!(
        resolve(vec![
            key_ident("mangle", "enabled"),
            key_ident("mangle", "disabled")
        ])
        .is_err()
    );
}

/// The argument grammar is shared with conditions, so every annotation that
/// takes plain words still has to reject the forms it has no meaning for.
#[test]
fn an_ordinary_annotation_rejects_condition_syntax() {
    let call = expr(AnnotationExprKind::Call {
        name: Ident("equals".into()),
        args: vec![number("1"), number("1")],
    });
    let inline = HirAnnotation {
        name: Ident("inline".into()),
        args: vec![HirAnnotationArg::Positional(call.clone())],
        span: Span::new(0, 0),
    };
    assert!(resolve_inline(&inline).is_err());
    assert!(resolve(vec![HirAnnotationArg::Positional(call)]).is_err());
    assert!(
        resolve(vec![HirAnnotationArg::KeyValue(
            Ident("name".into()),
            expr(AnnotationExprKind::Qualified {
                namespace: Ident("def".into()),
                name: Ident("symbol".into()),
            })
        )])
        .is_err()
    );
}
