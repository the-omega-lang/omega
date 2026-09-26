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
        &pair_pattern(TypePattern::Parameter(0), TypePattern::Fixed(ResolvedType::U8)),
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
