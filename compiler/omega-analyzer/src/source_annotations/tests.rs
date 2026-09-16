use super::*;
use omega_parser::SourceModule;

fn annotations(source: &str) -> Vec<AnnotationNode> {
    SourceModule::parse(source).unwrap().annotations
}

#[test]
fn conditions_validate_multiplicity_and_shape() {
    assert!(matches!(
        source_condition(&annotations("@[cond(false)] @[cond(true)]"))
            .unwrap_err()
            .kind,
        SourceAnnotationErrorKind::Duplicate { .. }
    ));
    for source in [
        "@[cond]",
        "@[cond()]",
        "@[cond(a = true)]",
        "@[cond(true, false)]",
    ] {
        assert!(matches!(
            source_condition(&annotations(source)).unwrap_err().kind,
            SourceAnnotationErrorKind::InvalidArguments { .. }
        ));
    }
}

#[test]
fn known_item_annotations_are_distinct_from_unknown_names() {
    for source in ["@[inline]", "@[layout]", "@[naked]", "@[symbol(export)]"] {
        assert!(matches!(
            resolve(&annotations(source)).1[0].kind,
            SourceAnnotationErrorKind::NotSourceLevel { .. }
        ));
    }
    assert!(matches!(
        resolve(&annotations("@[bogus]")).1[0].kind,
        SourceAnnotationErrorKind::Unknown { .. }
    ));
}

#[test]
fn suppression_takes_bare_names_once_per_source() {
    assert!(matches!(
        resolve(&annotations("@[suppress(a)] @[suppress(b)]")).1[0].kind,
        SourceAnnotationErrorKind::Duplicate { .. }
    ));
    assert!(matches!(
        resolve(&annotations("@[suppress(key = value)]")).1[0].kind,
        SourceAnnotationErrorKind::InvalidArguments { .. }
    ));
    let (resolved, errors) = resolve(&annotations("@[cond(true)] @[suppress(a, b)]"));
    assert!(errors.is_empty());
    assert_eq!(
        resolved
            .suppress
            .iter()
            .map(Ident::as_ref)
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}
