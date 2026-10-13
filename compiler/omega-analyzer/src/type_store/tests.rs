use super::*;
use crate::resolved_type::ResolvedType;
use omega_hir::ModuleId;

fn identity(local: u32, kind: NominalKind, name: &str) -> NominalIdentity {
    NominalIdentity {
        decl_id: HirId {
            module: ModuleId(0),
            local,
        },
        kind,
        name: Ident(name.into()),
        module_path: ModulePath::from(vec![Ident("pkg".into())]),
        generic_args: vec![],
        variant_names: vec![],
    }
}

fn struct_facts(fields: Vec<ResolvedField>) -> StructFacts {
    StructFacts {
        fields,
        layout: Layout::default(),
        suppress: vec![],
        is_marker: false,
    }
}

#[test]
fn each_intern_is_a_distinct_type() {
    let mut store = TypeStore::default();
    let first = store.intern(identity(0, NominalKind::Struct, "Pair"));
    let second = store.intern(identity(0, NominalKind::Struct, "Pair"));
    assert_ne!(first, second);
    assert_eq!(first, first.clone());
}

#[test]
fn reading_an_unwritten_stage_is_incomplete() {
    let mut store = TypeStore::default();
    let nominal = store.intern(identity(0, NominalKind::Struct, "Node"));

    let incomplete = store.struct_facts(&nominal).unwrap_err();
    assert_eq!(incomplete.stage, Stage::Definition);
    assert!(matches!(
        ResolveError::from(incomplete),
        ResolveError::Cycle(path) if path.len() == 1 && path[0].name.as_ref() == "Node"
    ));

    store.define_struct(&nominal, struct_facts(vec![]));
    assert!(store.struct_facts(&nominal).is_ok());
    assert_eq!(
        store.functions(&nominal).unwrap_err().stage,
        Stage::Functions
    );

    store.define_functions(&nominal, vec![]);
    assert!(store.functions(&nominal).unwrap().is_empty());
}

#[test]
#[should_panic(expected = "written twice")]
fn a_stage_is_written_once() {
    let mut store = TypeStore::default();
    let nominal = store.intern(identity(0, NominalKind::Struct, "Once"));
    store.define_struct(&nominal, struct_facts(vec![]));
    store.define_struct(&nominal, struct_facts(vec![]));
}

#[test]
fn layout_requires_every_inline_definition() {
    let mut store = TypeStore::default();
    let inner = store.intern(identity(0, NominalKind::Struct, "Inner"));
    let outer = store.intern(identity(1, NominalKind::Struct, "Outer"));
    store.define_struct(
        &outer,
        struct_facts(vec![
            ResolvedField::new(
                Ident("by_pointer".into()),
                ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::Struct(inner.clone())),
                    mutable: false,
                },
                Visibility::Exposed,
            ),
            ResolvedField::new(
                Ident("inline".into()),
                ResolvedType::Struct(inner.clone()),
                Visibility::Exposed,
            ),
        ]),
    );

    let outer_type = ResolvedType::Struct(outer);
    let missing = crate::layout::require_complete(&store, &outer_type).unwrap_err();
    assert_eq!(missing.nominal, inner);

    store.define_struct(&inner, struct_facts(vec![]));
    assert!(crate::layout::require_complete(&store, &outer_type).is_ok());
}
