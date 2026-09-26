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

fn expr(kind: omega_hir::AnnotationExprKind) -> omega_hir::HirAnnotationValue {
    omega_hir::HirAnnotationValue {
        kind,
        span: sp(),
        origin: Default::default(),
    }
}

fn ident_value(name: &str) -> omega_hir::HirAnnotationValue {
    expr(omega_hir::AnnotationExprKind::Name(Ident(name.into())))
}

fn string_value(text: &str) -> omega_hir::HirAnnotationValue {
    expr(omega_hir::AnnotationExprKind::Literal(
        omega_hir::AnnotationLiteral::Str(text.into()),
    ))
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
        string_value("raw_symbol"),
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
    let exported = symbol_annotation(vec![omega_hir::HirAnnotationArg::Positional(ident_value(
        "export",
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

fn hole_kinds(annotation: &Type) -> Option<(Type, Vec<(String, bool)>)> {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let holed = a.rewrite_annotation_holes(id(1), sp(), annotation)?;
    let kinds = holed
        .holes
        .iter()
        .map(|hole| (hole.ident.as_ref().to_string(), hole.is_comp()))
        .collect();
    Some((holed.rewritten, kinds))
}

#[test]
fn every_hole_becomes_a_fresh_parameter_of_its_kind() {
    let annotation = Type::Pointer(
        Box::new(Type::SizedArray(Box::new(Type::Infer), ArrayLength::Infer)),
        false,
    );
    let (rewritten, kinds) = hole_kinds(&annotation).expect("the annotation has holes");
    assert_eq!(
        kinds,
        vec![("$Hole0".to_string(), true), ("$Hole1".to_string(), false)]
    );
    let Type::Pointer(inner, false) = rewritten else {
        panic!("the pointer shape must survive rewriting");
    };
    assert_eq!(
        *inner,
        Type::SizedArray(
            Box::new(Type::Named(Ident("$Hole1".into()).into())),
            ArrayLength::Path(Ident("$Hole0".into()).into()),
        )
    );
}

#[test]
fn an_annotation_without_holes_is_not_rewritten() {
    assert!(hole_kinds(&Type::Named(Ident("i32".into()).into())).is_none());
}

#[test]
fn holes_in_spec_references_and_anonymous_enums_are_left_for_rejection() {
    for annotation in [
        Type::SpecStatic(vec![Type::Infer]),
        Type::AnonymousEnum(vec![Type::Named(Ident("i32".into()).into()), Type::Infer]),
    ] {
        assert!(hole_kinds(&annotation).is_none());
    }
}
