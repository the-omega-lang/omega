use super::*;

fn annotation(name: &str, args: Vec<HirAnnotationArg>) -> HirAnnotation {
    HirAnnotation {
        name: Ident(name.into()),
        args,
        span: Span::new(0, 0),
    }
}

fn ident_arg(name: &str) -> HirAnnotationArg {
    HirAnnotationArg::Ident(Ident(name.into()))
}

#[test]
fn mangling_enabled_and_disabled_parse_from_bare_idents() {
    assert_eq!(
        resolve_mangling(&annotation("mangling", vec![ident_arg("enabled")])),
        Ok(ManglingMode::Enabled)
    );
    assert_eq!(
        resolve_mangling(&annotation("mangling", vec![ident_arg("disabled")])),
        Ok(ManglingMode::Disabled)
    );
}

#[test]
fn mangling_force_takes_a_symbol_name() {
    let forced = annotation(
        "mangling",
        vec![HirAnnotationArg::KeyValue(
            Ident("force".into()),
            HirAnnotationValue::StrLiteral("my_symbol".into()),
        )],
    );
    assert_eq!(
        resolve_mangling(&forced),
        Ok(ManglingMode::Forced("my_symbol".into()))
    );

    let empty_name = annotation(
        "mangling",
        vec![HirAnnotationArg::KeyValue(
            Ident("force".into()),
            HirAnnotationValue::StrLiteral(String::new()),
        )],
    );
    assert!(resolve_mangling(&empty_name).is_err());
}

#[test]
fn mangling_rejects_unrecognized_arguments() {
    assert!(resolve_mangling(&annotation("mangling", vec![ident_arg("sometimes")])).is_err());
    assert!(resolve_mangling(&annotation("mangling", vec![])).is_err());
}
