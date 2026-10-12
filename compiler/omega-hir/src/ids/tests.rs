use super::*;

fn ident(name: &str) -> Ident {
    Ident(name.to_string())
}

fn module(names: &[&str]) -> ModulePath {
    ModulePath::from_names(names)
}

#[test]
fn module_paths_are_equal_by_content() {
    let written = module(&["pkg", "math"]);
    let built = ModulePath::new(vec![ident("pkg"), ident("math")]);
    assert_eq!(written, built);
    assert_ne!(written, module(&["pkg"]));

    let mut seen = std::collections::HashSet::new();
    seen.insert(written);
    assert!(seen.contains(&built));
    assert!(seen.contains([ident("pkg"), ident("math")].as_slice()));
}

#[test]
fn module_paths_order_segment_by_segment() {
    let mut paths = vec![
        module(&["pkg", "b"]),
        module(&["pkg"]),
        module(&["core", "z"]),
        module(&["pkg", "a"]),
    ];
    paths.sort();
    assert_eq!(
        paths,
        vec![
            module(&["core", "z"]),
            module(&["pkg"]),
            module(&["pkg", "a"]),
            module(&["pkg", "b"]),
        ]
    );
}

#[test]
fn paths_display_with_double_colons() {
    let math = module(&["pkg", "math"]);
    assert_eq!(math.to_string(), "pkg::math");
    assert_eq!(math.join(ident("sum")).to_string(), "pkg::math::sum");
    assert_eq!(ModulePath::default().join(ident("T")).to_string(), "T");
}

#[test]
fn module_navigation_keeps_module_and_item_apart() {
    let math = module(&["pkg", "math"]);
    assert_eq!(math.root(), Some(&ident("pkg")));
    assert_eq!(math.parent(), Some(module(&["pkg"])));
    assert_eq!(module(&["pkg"]).parent(), Some(ModulePath::default()));
    assert_eq!(ModulePath::default().parent(), None);
    assert_eq!(math.child(ident("vec")), module(&["pkg", "math", "vec"]));

    let sum = math.join(ident("sum"));
    assert_eq!(sum.module, math);
    assert_eq!(sum.name, ident("sum"));
    assert_eq!(sum.as_module(), module(&["pkg", "math", "sum"]));
    assert_eq!(math.as_item(), Some(module(&["pkg"]).join(ident("math"))));
}

#[test]
fn item_paths_split_written_segments_at_the_last_one() {
    let segments = [ident("pkg"), ident("math"), ident("sum")];
    let item = ItemPath::from_segments(&segments).expect("a non-empty path");
    assert_eq!(item, module(&["pkg", "math"]).join(ident("sum")));
    assert_eq!(item.to_segments(), segments);
    assert_eq!(ItemPath::from_segments(&[]), None);
    assert_eq!(
        module(&["pkg"]).join_segments(&[ident("math"), ident("sum")]),
        Some(item)
    );
    assert_eq!(module(&["pkg"]).join_segments(&[]), None);
}
