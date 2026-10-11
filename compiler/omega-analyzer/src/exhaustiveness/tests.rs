use super::*;

fn int_leaf() -> Rc<Leaf> {
    Rc::new(Leaf {
        lo: 0,
        hi: 255,
        kind: LeafKind::Integer,
    })
}

fn range(leaf: &Rc<Leaf>, lo: i128, hi: i128) -> Pat {
    Pat::Range {
        lo,
        hi,
        leaf: leaf.clone(),
    }
}

fn key(position: usize, name: &str) -> ComponentKey {
    ComponentKey {
        position,
        name: name.to_string(),
    }
}

fn point(x: Pat, y: Pat) -> Pat {
    Pat::Struct {
        name: Rc::from("Point"),
        fields: Product::new(vec![(key(0, "x"), x), (key(1, "y"), y)]),
    }
}

fn option() -> Rc<Sum> {
    Rc::new(Sum {
        names: vec!["Option::None".into(), "Option::Some".into()],
    })
}

fn some(value: Pat) -> Pat {
    Pat::Variant {
        sum: option(),
        index: 1,
        fields: Product::new(vec![(key(0, "value"), value)]),
    }
}

fn none() -> Pat {
    Pat::Variant {
        sum: option(),
        index: 0,
        fields: Product::default(),
    }
}

fn arm(pat: Pat) -> Arm {
    Arm {
        alternatives: vec![pat],
    }
}

fn remainder(arms: &[Pat]) -> Vec<String> {
    let mut space = vec![Pat::Any];
    for pat in arms {
        space = subtract(space, &arm(pat.clone()));
    }
    space.iter().map(render).collect()
}

#[test]
fn leaf_relations() {
    let leaf = int_leaf();
    assert_eq!(
        relate(&range(&leaf, 0, 9), &range(&leaf, 20, 30)),
        Relation::Disjoint
    );
    assert_eq!(
        relate(&range(&leaf, 0, 9), &range(&leaf, 0, 9)),
        Relation::Equal
    );
    assert_eq!(
        relate(&range(&leaf, 5, 5), &range(&leaf, 0, 9)),
        Relation::Sub
    );
    assert_eq!(
        relate(&range(&leaf, 0, 9), &range(&leaf, 5, 5)),
        Relation::Super
    );
    assert_eq!(
        relate(&range(&leaf, 0, 9), &range(&leaf, 5, 19)),
        Relation::Partial(vec![])
    );
    assert_eq!(relate(&Pat::Any, &range(&leaf, 0, 255)), Relation::Equal);
    assert_eq!(relate(&Pat::Any, &range(&leaf, 0, 1)), Relation::Super);
}

#[test]
fn product_combination() {
    let leaf = int_leaf();
    let zero = || range(&leaf, 0, 0);
    assert_eq!(
        relate(&point(zero(), Pat::Any), &point(Pat::Any, zero())),
        Relation::SameWeight
    );
    assert_eq!(
        relate(&point(zero(), zero()), &point(zero(), Pat::Any)),
        Relation::Sub
    );
    assert_eq!(
        relate(
            &point(zero(), range(&leaf, 0, 9)),
            &point(range(&leaf, 1, 1), range(&leaf, 5, 19))
        ),
        Relation::Disjoint,
        "a disjoint component outweighs a partial one"
    );
    assert_eq!(
        relate(
            &point(zero(), range(&leaf, 0, 9)),
            &point(Pat::Any, range(&leaf, 5, 19))
        ),
        Relation::Partial(vec!["y".into()])
    );
    assert_eq!(
        relate(&point(Pat::Any, Pat::Any), &Pat::Any),
        Relation::Equal
    );
}

#[test]
fn nested_mixed_propagates_as_same_weight() {
    let leaf = int_leaf();
    let a = some(point(range(&leaf, 0, 0), Pat::Any));
    let b = some(point(Pat::Any, range(&leaf, 0, 0)));
    assert_eq!(relate(&a, &b), Relation::SameWeight);
    assert_eq!(relate(&some(Pat::Any), &none()), Relation::Disjoint);
    assert_eq!(relate(&Pat::Any, &none()), Relation::Super);
}

#[test]
fn interval_subtraction() {
    let leaf = int_leaf();
    assert_eq!(remainder(&[range(&leaf, 0, 0)]), ["1..=255"]);
    assert_eq!(remainder(&[range(&leaf, 5, 9)]), ["0..=4", "10..=255"]);
    assert_eq!(
        remainder(&[range(&leaf, 0, 99), range(&leaf, 100, 255)]),
        Vec::<String>::new()
    );
}

#[test]
fn empty_patterns_never_intersect_or_change_coverage() {
    let leaf = int_leaf();
    for empty in [
        range(&leaf, 5, 4),
        point(range(&leaf, 5, 4), Pat::Any),
        some(point(range(&leaf, 5, 4), Pat::Any)),
        Pat::Member {
            sum: Rc::new(Sum {
                names: vec!["Point".into(), "*str".into()],
            }),
            index: 0,
            inner: Box::new(point(range(&leaf, 5, 4), Pat::Any)),
        },
    ] {
        assert_eq!(relate(&Pat::Any, &empty), Relation::Disjoint);
        assert_eq!(relate(&empty, &Pat::Any), Relation::Disjoint);
        assert!(intersect(&Pat::Any, &empty).is_none());
        assert!(intersect(&empty, &Pat::Any).is_none());
        assert!(!arm_intersects(&[Pat::Any], &arm(empty.clone())));
        assert_eq!(remainder(&[empty]), [".."]);
    }
}

#[test]
fn nested_subtraction_and_rendering() {
    let leaf = int_leaf();
    let missing = remainder(&[none(), some(point(range(&leaf, 0, 0), Pat::Any))]);
    assert_eq!(
        missing,
        ["Option::Some { value = Point { x = 1..=255; .. }; .. }"]
    );
    let missing = remainder(&[some(Pat::Any)]);
    assert_eq!(missing, ["Option::None"]);
}

#[test]
fn union_of_earlier_arms_covers_later_arm() {
    let flag = Rc::new(Leaf {
        lo: 0,
        hi: 1,
        kind: LeafKind::Bool,
    });
    let pair = |a: Pat| Pat::Struct {
        name: Rc::from("Pair"),
        fields: Product::new(vec![(key(0, "a"), a)]),
    };
    let mut space = vec![Pat::Any];
    for pat in [pair(range(&flag, 1, 1)), pair(range(&flag, 0, 0))] {
        space = subtract(space, &arm(pat));
    }
    assert!(!arm_intersects(&space, &arm(pair(Pat::Any))));
}

#[test]
fn members_render_their_type_or_inner_pattern() {
    let sum = Rc::new(Sum {
        names: vec!["Point".into(), "*str".into()],
    });
    let leaf = int_leaf();
    let member = Pat::Member {
        sum: sum.clone(),
        index: 0,
        inner: Box::new(point(range(&leaf, 0, 0), Pat::Any)),
    };
    assert_eq!(remainder(&[member]), ["Point { x = 1..=255; .. }", "*str"]);
}

#[test]
fn leaf_rendering() {
    assert_eq!(render_interval(LeafKind::Bool, 0, 0), "false");
    assert_eq!(
        render_interval(LeafKind::Char, 'a' as i128, 'z' as i128),
        "'a'..='z'"
    );
    assert_eq!(render_interval(LeafKind::Char, 0, 0), "U+0000");
}
