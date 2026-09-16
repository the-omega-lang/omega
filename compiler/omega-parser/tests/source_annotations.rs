use omega_parser::{SourceModule, diagnostics::ParseErrorKind};

#[test]
fn source_prologues_are_separate_from_items() {
    let module =
        SourceModule::parse("@[cond(true)]\n@[suppress(a, b)]\n@cond(true)\nx := 1;").unwrap();
    assert_eq!(module.annotations.len(), 2);
    assert_eq!(module.annotations[0].name.as_ref(), "cond");
    assert_eq!(module.nodes.len(), 1);
    assert_eq!(module.nodes[0].conditions.len(), 1);
    for source in ["@[name]", "@[name()]"] {
        let module = SourceModule::parse(source).unwrap();
        assert!(module.nodes.is_empty());
        assert!(module.annotations[0].args.is_empty());
    }
}

#[test]
fn source_annotations_are_rejected_outside_the_prologue() {
    for source in [
        "x := 1; @[cond(true)] y := 2;",
        "struct S { @[suppress(a)] x: i32; }",
        "f() => void { @[suppress(a)] x := 1; }",
        "macro m() => { @[cond(true)] x := 1; }",
        "@cond(true) @[cond(true)] x := 1;",
    ] {
        let errors = SourceModule::parse(source).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| matches!(error.kind, ParseErrorKind::SourceAnnotationNotInPrologue)),
            "{source}: {errors:?}"
        );
    }
}
