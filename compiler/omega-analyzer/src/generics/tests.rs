use super::pattern::{ArgumentPattern, TypePattern};
use super::*;
use omega_parser::prelude::Path;

fn named(name: &str) -> Type {
    Type::Named(Path::from(Ident(name.into())))
}

fn pair(a: Type, b: Type) -> Type {
    Type::Generic(
        Path::from(Ident("Pair".into())),
        vec![GenericArg::Type(a), GenericArg::Type(b)],
    )
}

fn pair_pattern(a: TypePattern, b: TypePattern) -> TypePattern {
    TypePattern::Nominal(
        vec![Ident("Pair".into())],
        vec![
            ArgumentPattern::Type(Box::new(a)),
            ArgumentPattern::Type(Box::new(b)),
        ],
    )
}

fn unify(params: &[HirGenericParam], raw: &Type, expected: &TypePattern) -> GenericSubstitution {
    let comp_types = vec![None; params.len()];
    let generics = GenericParams {
        params,
        comp_types: &comp_types,
        pointer_bits: 64,
    };
    let mut subst = GenericSubstitution::new();
    unify_generic_pattern(&generics, raw, expected, &mut subst);
    subst
}

#[test]
fn a_known_part_binds_the_parameter_it_meets() {
    let params = [HirGenericParam::r#type(Ident("T".into()), vec![], None)];
    let subst = unify(
        &params,
        &pair(named("T"), named("T")),
        &pair_pattern(
            TypePattern::Parameter(0),
            TypePattern::Fixed(ResolvedType::U8),
        ),
    );
    assert_eq!(
        subst.get(&Ident("T".into())),
        Some(&ResolvedGenericArg::Type(ResolvedType::U8))
    );
}

#[test]
fn a_hole_binds_nothing() {
    let params = [HirGenericParam::r#type(Ident("T".into()), vec![], None)];
    let subst = unify(
        &params,
        &pair(named("T"), named("T")),
        &pair_pattern(TypePattern::Parameter(0), TypePattern::Parameter(1)),
    );
    assert!(subst.is_empty());
}

#[test]
fn structure_is_followed_through_pointers() {
    let params = [HirGenericParam::r#type(Ident("T".into()), vec![], None)];
    let subst = unify(
        &params,
        &Type::Pointer(Box::new(pair(named("T"), named("u8"))), false),
        &TypePattern::Pointer(
            Box::new(pair_pattern(
                TypePattern::Fixed(ResolvedType::I64),
                TypePattern::Parameter(0),
            )),
            false,
        ),
    );
    assert_eq!(
        subst.get(&Ident("T".into())),
        Some(&ResolvedGenericArg::Type(ResolvedType::I64))
    );
}

fn ty(pattern: TypePattern) -> ArgumentPattern {
    ArgumentPattern::Type(Box::new(pattern))
}

fn usize_value(value: i128) -> ArgumentPattern {
    ArgumentPattern::Value(CompScalar::Int {
        r#type: crate::resolved_type::CompIntType::USize,
        value,
    })
}

fn seed(
    callee: &TypePattern,
    expected: &TypePattern,
    mut bindings: Vec<Option<ResolvedGenericArg>>,
) -> Vec<Option<ResolvedGenericArg>> {
    callee.infer_from_pattern(expected, &mut bindings);
    bindings
}

fn bound(r#type: ResolvedType) -> Option<ResolvedGenericArg> {
    Some(ResolvedGenericArg::Type(r#type))
}

/// The expectation's `Parameter(0)` is a hole, not the callee's `T`.
#[test]
fn expectation_parameters_are_their_own_namespace() {
    let seeded = seed(
        &pair_pattern(TypePattern::Parameter(0), TypePattern::Parameter(1)),
        &pair_pattern(
            TypePattern::Parameter(0),
            TypePattern::Fixed(ResolvedType::U8),
        ),
        vec![None, None],
    );
    assert_eq!(seeded, vec![None, bound(ResolvedType::U8)]);
}

#[test]
fn a_repeated_parameter_is_bound_by_any_known_occurrence() {
    let seeded = seed(
        &pair_pattern(TypePattern::Parameter(0), TypePattern::Parameter(0)),
        &pair_pattern(
            TypePattern::Parameter(0),
            TypePattern::Fixed(ResolvedType::U8),
        ),
        vec![None],
    );
    assert_eq!(seeded, vec![bound(ResolvedType::U8)]);
}

#[test]
fn a_written_binding_is_kept() {
    let seeded = seed(
        &pair_pattern(TypePattern::Parameter(0), TypePattern::Parameter(1)),
        &pair_pattern(
            TypePattern::Fixed(ResolvedType::U8),
            TypePattern::Fixed(ResolvedType::U16),
        ),
        vec![bound(ResolvedType::I64), None],
    );
    assert_eq!(
        seeded,
        vec![bound(ResolvedType::I64), bound(ResolvedType::U16)]
    );
}

/// A callee parameter takes only a complete type, never a shape with holes.
#[test]
fn a_partial_expectation_does_not_bind_a_whole_parameter() {
    let seeded = seed(
        &TypePattern::Parameter(0),
        &pair_pattern(
            TypePattern::Parameter(0),
            TypePattern::Fixed(ResolvedType::U8),
        ),
        vec![None],
    );
    assert_eq!(seeded, vec![None]);
}

#[test]
fn structure_is_followed_through_pointers_and_function_types() {
    let callee = TypePattern::Pointer(
        Box::new(TypePattern::Function(
            vec![TypePattern::Parameter(0)],
            Box::new(pair_pattern(
                TypePattern::Parameter(1),
                TypePattern::Parameter(2),
            )),
            crate::resolved_type::CallingConvention::Omega,
            false,
        )),
        false,
    );
    let expected = TypePattern::Pointer(
        Box::new(TypePattern::Function(
            vec![TypePattern::Fixed(ResolvedType::I16)],
            Box::new(pair_pattern(
                TypePattern::Parameter(0),
                TypePattern::Fixed(ResolvedType::Bool),
            )),
            crate::resolved_type::CallingConvention::Omega,
            false,
        )),
        false,
    );
    assert_eq!(
        seed(&callee, &expected, vec![None, None, None]),
        vec![bound(ResolvedType::I16), None, bound(ResolvedType::Bool)]
    );
}

#[test]
fn a_different_nominal_binds_nothing() {
    let other = TypePattern::Nominal(
        vec![Ident("Other".into())],
        vec![
            ty(TypePattern::Parameter(0)),
            ty(TypePattern::Fixed(ResolvedType::U8)),
        ],
    );
    assert_eq!(
        seed(
            &pair_pattern(TypePattern::Parameter(0), TypePattern::Parameter(1)),
            &other,
            vec![None, None],
        ),
        vec![None, None]
    );
}

#[test]
fn known_values_seed_comp_parameters() {
    let usize_kind = CompScalarType::Int(crate::resolved_type::CompIntType::USize);
    let buffer = |args| TypePattern::Nominal(vec![Ident("Buffer".into())], args);
    let seeded = seed(
        &buffer(vec![
            ArgumentPattern::Parameter(0, usize_kind),
            ty(TypePattern::Parameter(1)),
        ]),
        &buffer(vec![usize_value(3), ty(TypePattern::Parameter(0))]),
        vec![None, None],
    );
    assert_eq!(
        seeded,
        vec![
            Some(ResolvedGenericArg::Comp(CompScalar::Int {
                r#type: crate::resolved_type::CompIntType::USize,
                value: 3,
            })),
            None,
        ]
    );

    let seeded = seed(
        &TypePattern::SizedArray(
            Box::new(TypePattern::Parameter(1)),
            ArgumentPattern::Parameter(0, usize_kind),
        ),
        &TypePattern::SizedArray(
            Box::new(TypePattern::Fixed(ResolvedType::U8)),
            usize_value(2),
        ),
        vec![None, None],
    );
    assert_eq!(
        seeded,
        vec![
            Some(ResolvedGenericArg::Comp(CompScalar::Int {
                r#type: crate::resolved_type::CompIntType::USize,
                value: 2,
            })),
            bound(ResolvedType::U8),
        ]
    );
}

/// A value slot and a type slot at the same position mean the shapes are
/// different declarations' applications; nothing is taken from either.
#[test]
fn mismatched_argument_kinds_bind_nothing() {
    let usize_kind = CompScalarType::Int(crate::resolved_type::CompIntType::USize);
    let callee = TypePattern::Nominal(
        vec![Ident("Buffer".into())],
        vec![
            ArgumentPattern::Parameter(0, usize_kind),
            ty(TypePattern::Parameter(1)),
        ],
    );
    let expected = TypePattern::Nominal(
        vec![Ident("Buffer".into())],
        vec![
            ty(TypePattern::Fixed(ResolvedType::U8)),
            ty(TypePattern::Fixed(ResolvedType::U8)),
        ],
    );
    assert_eq!(seed(&callee, &expected, vec![None, None]), vec![None, None]);
}
