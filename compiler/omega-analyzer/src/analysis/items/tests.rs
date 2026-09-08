use super::*;
use crate::analysis::tests::{NoResolver, analyzer, id, sp};
use crate::annotations::{ManglingMode, SymbolPolicy, SymbolVisibility};

fn annotation(name: &str, args: Vec<omega_hir::HirAnnotationArg>) -> omega_hir::HirAnnotation {
    omega_hir::HirAnnotation {
        name: Ident(name.into()),
        args,
        span: sp(),
    }
}

fn symbol_annotation(args: Vec<omega_hir::HirAnnotationArg>) -> omega_hir::HirAnnotation {
    annotation("symbol", args)
}

fn ident_value(name: &str) -> omega_hir::HirAnnotationValue {
    omega_hir::HirAnnotationValue::Ident(Ident(name.into()))
}

/// `foreign` is already the statement that a symbol comes from, or is meant
/// for, outside this compilation, so it forks the visibility default the same
/// way it forks the naming one.
#[test]
fn foreign_items_default_to_the_source_name_and_a_cross_image_symbol() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        &[],
        crate::annotations::ItemKind::ForeignFunction,
        false,
        false,
        SymbolPolicy::foreign(),
    );
    assert_eq!(resolved.symbol, SymbolPolicy::foreign());
    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

#[test]
fn ordinary_items_default_to_mangled_and_hidden() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        &[],
        crate::annotations::ItemKind::Function,
        false,
        false,
        SymbolPolicy::ordinary(),
    );
    assert_eq!(resolved.symbol, SymbolPolicy::ordinary());
}

#[test]
fn an_explicit_symbol_annotation_overrides_the_foreign_default() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let enabled = symbol_annotation(vec![omega_hir::HirAnnotationArg::KeyValue(
        Ident("mangle".into()),
        ident_value("enabled"),
    )]);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        std::slice::from_ref(&enabled),
        crate::annotations::ItemKind::ForeignFunction,
        false,
        false,
        SymbolPolicy::foreign(),
    );
    assert_eq!(
        resolved.symbol,
        SymbolPolicy {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Default,
        },
        "an explicit naming mode leaves the foreign visibility default alone"
    );

    let named = symbol_annotation(vec![omega_hir::HirAnnotationArg::KeyValue(
        Ident("name".into()),
        omega_hir::HirAnnotationValue::StrLiteral("raw_symbol".into()),
    )]);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(3),
        std::slice::from_ref(&named),
        crate::annotations::ItemKind::ForeignFunction,
        false,
        false,
        SymbolPolicy::foreign(),
    );
    assert_eq!(
        resolved.symbol,
        SymbolPolicy {
            mangling: ManglingMode::Forced("raw_symbol".into()),
            visibility: SymbolVisibility::Default,
        }
    );
}

/// `export` decides visibility only, so an omitted `mangle`/`name` still
/// resolves to the caller's item default rather than the ordinary one.
#[test]
fn export_alone_keeps_the_item_naming_default() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let exported = symbol_annotation(vec![omega_hir::HirAnnotationArg::Ident(Ident(
        "export".into(),
    ))]);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        std::slice::from_ref(&exported),
        crate::annotations::ItemKind::Function,
        false,
        false,
        SymbolPolicy::ordinary(),
    );
    assert_eq!(
        resolved.symbol,
        SymbolPolicy {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Default,
        }
    );
    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

/// The one thing `export` still decides on a foreign item: a declaration that
/// is resolved inside this image after all can say so.
#[test]
fn a_foreign_item_can_opt_back_into_a_hidden_symbol() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let unexported = symbol_annotation(vec![omega_hir::HirAnnotationArg::KeyValue(
        Ident("export".into()),
        ident_value("disabled"),
    )]);
    let resolved = crate::annotations::resolve(
        &mut a,
        id(2),
        std::slice::from_ref(&unexported),
        crate::annotations::ItemKind::ForeignBinding,
        false,
        false,
        SymbolPolicy::foreign(),
    );
    assert_eq!(
        resolved.symbol,
        SymbolPolicy::mangled(ManglingMode::Disabled)
    );
    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

/// A macro can expand to an identifier-valued argument, so the analyzer must
/// still reject one where only an integer or `sizeof` is meaningful.
#[test]
fn an_identifier_value_is_rejected_by_layout() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let layout = annotation(
        "layout",
        vec![omega_hir::HirAnnotationArg::KeyValue(
            Ident("align".into()),
            ident_value("enabled"),
        )],
    );
    crate::annotations::resolve(
        &mut a,
        id(2),
        std::slice::from_ref(&layout),
        crate::annotations::ItemKind::Struct,
        false,
        false,
        SymbolPolicy::ordinary(),
    );
    let (errors, _, _) = a.finish();
    assert_eq!(errors.len(), 1);
}
