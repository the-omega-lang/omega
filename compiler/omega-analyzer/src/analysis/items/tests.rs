use super::*;
use crate::resolved_type::ResolvedConformance;
use omega_hir::ModuleId;

/// A `ModuleResolver` that panics if the analyzer ever consults it. The
/// behavior under test here (mangling defaults, foreign-aggregate rejection)
/// never resolves imports, calls, or generics, so this stub only needs to
/// satisfy the trait, not implement it.
struct NoResolver;

impl ModuleResolver for NoResolver {
    fn macro_origin_module(&self, _origin: Origin) -> Option<Vec<Ident>> {
        None
    }

    fn macro_origin_visibility(&self, _origin: Origin) -> Option<Visibility> {
        None
    }

    fn declared_item_visibility(&mut self, _absolute_path: &[Ident]) -> Option<Visibility> {
        None
    }

    fn resolve_import_alias(
        &mut self,
        _module_path: &[Ident],
        _alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        unreachable!("test never triggers import resolution")
    }

    fn ambient_core_candidates(
        &mut self,
        _accessor: &[Ident],
        _name: &Ident,
    ) -> Result<Option<Vec<Ident>>, ResolveError> {
        unreachable!("test never triggers import resolution")
    }

    fn import_alias_names(&mut self, _module_path: &[Ident]) -> Vec<Ident> {
        vec![]
    }

    fn raw_import_absolute_path(
        &mut self,
        _module_path: &[Ident],
        _alias: &Ident,
    ) -> Result<Option<(Vec<Ident>, bool)>, ResolveError> {
        unreachable!("test never triggers import resolution")
    }

    fn resolve_item(
        &mut self,
        _accessor_module_path: &[Ident],
        _absolute_path: &[Ident],
        _type_args: &[ResolvedType],
        _options: ResolveItemOptions,
    ) -> Result<ResolvedItem, ResolveError> {
        unreachable!("test never triggers item resolution")
    }

    fn is_item_visible(&mut self, _accessor_module_path: &[Ident], _absolute_path: &[Ident]) -> bool {
        true
    }

    fn generic_function_signature(
        &mut self,
        _absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError> {
        unreachable!("test never triggers generic resolution")
    }

    fn generic_literal_signature(
        &mut self,
        _absolute_path: &[Ident],
        _variant: Option<&Ident>,
    ) -> Result<Option<GenericLiteralSignature>, ResolveError> {
        unreachable!("test never triggers generic resolution")
    }

    fn generic_static_function_signature(
        &mut self,
        _owner_absolute: &[Ident],
        _function_name: &Ident,
    ) -> Result<Option<GenericStaticFunctionSignature>, ResolveError> {
        unreachable!("test never triggers generic resolution")
    }

    fn function_overload_signatures(
        &mut self,
        _module_path: &[Ident],
        _name: &Ident,
    ) -> Result<Option<OverloadCandidates>, ResolveError> {
        unreachable!("test never triggers overload resolution")
    }

    fn fresh_synthetic_id(&mut self) -> HirId {
        unreachable!("test never requests a synthetic id")
    }

    fn similar_item_name(
        &mut self,
        _module_path: &[Ident],
        _target: &Ident,
        _namespace: ItemNamespace,
    ) -> Option<Ident> {
        None
    }

    fn spec_declaration(
        &mut self,
        _absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError> {
        unreachable!("test never triggers spec resolution")
    }

    fn primitive_methods(
        &mut self,
        _receiver: &ResolvedType,
    ) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError> {
        unreachable!("test never triggers primitive method lookup")
    }

    fn conformance_for(
        &mut self,
        _target: &ResolvedType,
        _spec: &Rc<RefCell<ResolvedSpecType>>,
        _spec_args: &[ResolvedType],
    ) -> Result<Option<ResolvedConformance>, ResolveError> {
        unreachable!("test never triggers conformance lookup")
    }

    fn conformances_for_type(
        &mut self,
        _target: &ResolvedType,
    ) -> Result<Vec<ResolvedConformance>, ResolveError> {
        unreachable!("test never triggers conformance lookup")
    }

    fn conformances_for_specs(
        &mut self,
        _target: &ResolvedType,
        _spec_ids: &[HirId],
    ) -> Result<Vec<ResolvedConformance>, ResolveError> {
        unreachable!("test never triggers conformance lookup")
    }

    fn resolve_function_body(
        &mut self,
        _decl_id: HirId,
    ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
        unreachable!("test never resolves a function body")
    }

    fn resolve_comp_value(&mut self, _decl_id: HirId) -> Option<ConstValue> {
        None
    }
}

fn id(n: u32) -> HirId {
    HirId {
        module: ModuleId(0),
        local: n,
    }
}

fn sp() -> Span {
    Span::new(0, 0)
}

fn analyzer(resolver: &mut NoResolver) -> Analyzer<'_> {
    Analyzer::new(
        resolver,
        vec![],
        &[],
        AnalysisSite::new(id(0), sp()),
        Target::DEFAULT,
    )
}

fn annotation(name: &str, args: Vec<omega_hir::HirAnnotationArg>) -> omega_hir::HirAnnotation {
    omega_hir::HirAnnotation {
        name: Ident(name.into()),
        args,
        span: sp(),
    }
}

fn dummy_struct_type() -> ResolvedType {
    ResolvedType::Struct(Rc::new(RefCell::new(ResolvedStructType {
        id: id(1),
        name: Ident("S".into()),
        module_path: vec![],
        type_args: vec![],
        fields: vec![],
        functions: vec![],
        layout: crate::annotations::Layout::default(),
        suppress: vec![],
        is_marker: false,
    })))
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
    assert_eq!(resolved.mangling, crate::annotations::ManglingMode::Disabled);
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

#[test]
fn reject_foreign_aggregate_by_value_accepts_scalars() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);

    let scalars = [ResolvedType::I32, ResolvedType::Bool];
    let ok = a.reject_foreign_aggregate_by_value(id(2), sp(), scalars.iter());
    assert!(ok);

    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

#[test]
fn reject_foreign_aggregate_by_value_flags_struct_by_value() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);

    let aggregate = dummy_struct_type();
    let ok = a.reject_foreign_aggregate_by_value(id(2), sp(), std::iter::once(&aggregate));
    assert!(!ok);

    let (errors, _, _) = a.finish();
    assert_eq!(errors.len(), 1);
    assert!(matches!(
        errors[0].kind,
        AnalysisErrorKind::ForeignAggregateByValue { .. }
    ));
}
