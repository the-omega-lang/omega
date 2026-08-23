use super::*;

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
