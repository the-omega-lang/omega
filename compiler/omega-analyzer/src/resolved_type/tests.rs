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
