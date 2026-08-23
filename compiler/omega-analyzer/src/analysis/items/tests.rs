use super::*;
use crate::analysis::tests::{NoResolver, analyzer, id, sp};

fn annotation(name: &str, args: Vec<omega_hir::HirAnnotationArg>) -> omega_hir::HirAnnotation {
    omega_hir::HirAnnotation {
        name: Ident(name.into()),
        args,
        span: sp(),
    }
}

#[test]
fn foreign_items_default_mangling_to_disabled_unless_overridden() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        &[],
        crate::annotations::ItemKind::ForeignFunction,
        false,
        false,
        crate::annotations::ManglingMode::Disabled,
    );
    assert_eq!(
        resolved.mangling,
        crate::annotations::ManglingMode::Disabled
    );
    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

#[test]
fn ordinary_items_default_mangling_to_enabled_and_are_unaffected() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        &[],
        crate::annotations::ItemKind::Function,
        false,
        false,
        crate::annotations::ManglingMode::Enabled,
    );
    assert_eq!(resolved.mangling, crate::annotations::ManglingMode::Enabled);
}

#[test]
fn explicit_mangling_annotation_overrides_the_foreign_default() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let enabled = annotation(
        "mangling",
        vec![omega_hir::HirAnnotationArg::Ident(Ident("enabled".into()))],
    );
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        std::slice::from_ref(&enabled),
        crate::annotations::ItemKind::ForeignFunction,
        false,
        false,
        crate::annotations::ManglingMode::Disabled,
    );
    assert_eq!(resolved.mangling, crate::annotations::ManglingMode::Enabled);

    let forced = annotation(
        "mangling",
        vec![omega_hir::HirAnnotationArg::KeyValue(
            Ident("force".into()),
            omega_hir::HirAnnotationValue::StrLiteral("raw_symbol".into()),
        )],
    );
    let resolved = crate::annotations::resolve(
        &mut a,
        id(3),
        std::slice::from_ref(&forced),
        crate::annotations::ItemKind::ForeignFunction,
        false,
        false,
        crate::annotations::ManglingMode::Disabled,
    );
    assert_eq!(
        resolved.mangling,
        crate::annotations::ManglingMode::Forced("raw_symbol".into())
    );
}
