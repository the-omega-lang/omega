use super::*;
use crate::resolved_type::ResolvedConformance;
use crate::resolver::ResolveError;
use omega_hir::ModuleId;

/// A `ModuleResolver` that panics if the analyzer ever consults it. Shared by
/// the `analysis` unit tests, whose subjects (mangling defaults, ABI
/// classification, conversion costs) never resolve imports, calls, or
/// generics, so this stub only needs to satisfy the trait, not implement it.
pub(crate) struct NoResolver;

impl ModuleResolver for NoResolver {
    fn macro_origin_module(&self, _origin: Origin) -> Option<Vec<Ident>> {
        None
    }

    fn resolve_explicit_anchor(
        &self,
        _origin_module: &[Ident],
        _path: &omega_parser::prelude::Path,
    ) -> Option<Result<Vec<Ident>, ResolveError>> {
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

    fn resolve_visible_alias(
        &mut self,
        _accessor: &[Ident],
        _alias_module: &[Ident],
        _name: &Ident,
        _bypass_visibility: bool,
    ) -> Result<Option<crate::resolver::ResolvedAlias>, ResolveError> {
        Ok(None)
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

    fn resolve_module_path(
        &mut self,
        _accessor: &[Ident],
        _absolute_path: &[Ident],
    ) -> Result<Option<Vec<Ident>>, ResolveError> {
        Ok(None)
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

    fn is_item_visible(
        &mut self,
        _accessor_module_path: &[Ident],
        _absolute_path: &[Ident],
    ) -> bool {
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

    fn generic_owner_function_signature(
        &mut self,
        _owner_absolute: &[Ident],
        _function_name: &Ident,
        _namespace: crate::resolved_type::FunctionNamespace,
    ) -> Result<Option<GenericOwnerFunctionSignature>, ResolveError> {
        unreachable!("test never triggers generic resolution")
    }

    fn resolve_overload_set(
        &mut self,
        _accessor: &[Ident],
        _access: &crate::resolver::ItemAccess,
    ) -> Result<Option<crate::resolver::ResolvedOverloadSet>, ResolveError> {
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

pub(crate) fn id(n: u32) -> HirId {
    HirId {
        module: ModuleId(0),
        local: n,
    }
}

pub(crate) fn sp() -> Span {
    Span::new(0, 0)
}

pub(crate) fn analyzer(resolver: &mut NoResolver) -> Analyzer<'_> {
    Analyzer::new(
        resolver,
        vec![],
        &[],
        AnalysisSite::new(id(0), sp()),
        Target::DEFAULT,
    )
}

pub(crate) fn dummy_struct_type() -> ResolvedType {
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

fn anonymous(members: Vec<ResolvedType>) -> ResolvedType {
    ResolvedType::AnonymousEnum {
        shape: Rc::new(ResolvedAnonymousEnum::canonicalize(members)),
        variant: None,
    }
}

/// Overload viability ranks with `conversion_cost` while argument checking
/// runs `convert_to_anonymous_enum`, so the two must agree on exactly which
/// values reach an anonymous enum, and exact acceptance must stay cheapest.
#[test]
fn conversion_cost_ranks_every_anonymous_enum_conversion_below_exact_acceptance() {
    let narrow = anonymous(vec![ResolvedType::I32, ResolvedType::Bool]);
    let wide = anonymous(vec![
        ResolvedType::I32,
        ResolvedType::Bool,
        ResolvedType::Char,
    ]);

    let exact = Analyzer::conversion_cost(&narrow, &narrow).expect("a shape accepts itself");
    let injection = Analyzer::conversion_cost(&narrow, &ResolvedType::I32)
        .expect("a member value injects into its enum");
    let widening =
        Analyzer::conversion_cost(&wide, &narrow).expect("a subset shape widens into a superset");
    assert_eq!(exact, 0);
    assert!(injection > exact && widening > exact);

    assert_eq!(Analyzer::conversion_cost(&narrow, &wide), None);
    assert_eq!(
        Analyzer::conversion_cost(
            &narrow,
            &anonymous(vec![ResolvedType::I32, ResolvedType::Char])
        ),
        None
    );
    assert_eq!(Analyzer::conversion_cost(&narrow, &ResolvedType::U8), None);
}

/// A refined read converts as its proven leaf, whether the destination is
/// the member's own type or any anonymous enum holding it.
#[test]
fn conversion_cost_sees_a_refined_read_as_its_proven_member() {
    let shape = Rc::new(ResolvedAnonymousEnum::canonicalize(vec![
        ResolvedType::I32,
        ResolvedType::Bool,
    ]));
    let parent = ResolvedType::AnonymousEnum {
        shape: shape.clone(),
        variant: None,
    };
    let refined = ResolvedType::AnonymousEnum {
        shape,
        variant: Some(shape_index(&parent, &ResolvedType::I32)),
    };

    assert_eq!(Analyzer::conversion_cost(&parent, &refined), Some(0));
    assert!(Analyzer::conversion_cost(&ResolvedType::I32, &refined).is_some());
    assert!(
        Analyzer::conversion_cost(
            &anonymous(vec![ResolvedType::I32, ResolvedType::Char]),
            &refined
        )
        .is_some()
    );
    assert_eq!(
        Analyzer::conversion_cost(&ResolvedType::Bool, &refined),
        None
    );
}

fn shape_index(parent: &ResolvedType, member: &ResolvedType) -> usize {
    let ResolvedType::AnonymousEnum { shape, .. } = parent else {
        panic!("not an anonymous enum: {parent}")
    };
    shape.index_of(member).expect("member of this shape")
}
