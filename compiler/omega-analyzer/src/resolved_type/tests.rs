use super::*;

fn fn_type(convention: CallingConvention) -> ResolvedFunctionType {
    ResolvedFunctionType {
        params: vec![(Ident("x".into()), ResolvedType::I32)],
        return_type: Box::new(ResolvedType::I32),
        is_variadic: false,
        self_mode: None,
        calling_convention: convention,
    }
}

#[test]
fn calling_convention_variants_are_pairwise_distinct() {
    assert_ne!(CallingConvention::Omega, CallingConvention::C);
    assert_ne!(CallingConvention::Omega, CallingConvention::SysV64);
    assert_ne!(CallingConvention::C, CallingConvention::SysV64);
}

#[test]
fn function_types_differing_only_in_calling_convention_are_unequal() {
    // `foreign(c) (i32) => i32` and `(i32) => i32` must stay distinct types
    // even on a target where both currently lower to the same machine
    // convention -- see docs/language/foreign-function-interface.md.
    let omega = fn_type(CallingConvention::Omega);
    let c = fn_type(CallingConvention::C);
    let sysv64 = fn_type(CallingConvention::SysV64);
    assert_ne!(omega, c);
    assert_ne!(omega, sysv64);
    assert_ne!(c, sysv64);
}

#[test]
fn function_pointer_assignment_rejects_calling_convention_mismatch() {
    let expected = ResolvedType::Function(fn_type(CallingConvention::Omega));
    let found = ResolvedType::Function(fn_type(CallingConvention::C));
    assert!(!expected.accepts(&found));
    assert!(expected.accepts(&ResolvedType::Function(fn_type(CallingConvention::Omega))));
}

fn spec_cell(id: u32, name: &str) -> Rc<RefCell<ResolvedSpecType>> {
    Rc::new(RefCell::new(ResolvedSpecType {
        id: HirId {
            module: omega_hir::ModuleId(0),
            local: id,
        },
        name: Ident(name.to_string()),
        visibility: Visibility::Exposed,
        generics: vec![],
        module_path: vec![],
        type_args: vec![],
        is_object_safe: true,
        functions: vec![],
        suppress: vec![],
    }))
}

#[test]
fn spec_shape_canonicalizes_reordered_members_identically() {
    let a = spec_cell(1, "A");
    let b = spec_cell(2, "B");
    let ab = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(a.clone(), vec![]),
        ResolvedSpecApplication::new(b.clone(), vec![]),
    ]);
    let ba = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(b, vec![]),
        ResolvedSpecApplication::new(a, vec![]),
    ]);
    assert_eq!(ab, ba);
    assert_eq!(ab.to_string(), "A + B");
}

#[test]
fn spec_shape_canonicalizes_duplicate_members_away() {
    let a1 = spec_cell(1, "A");
    let a2 = spec_cell(1, "A");
    let shape = ResolvedSpecShape::canonicalize(vec![
        ResolvedSpecApplication::new(a1, vec![]),
        ResolvedSpecApplication::new(a2, vec![]),
    ]);
    assert_eq!(shape.members.len(), 1);
}
