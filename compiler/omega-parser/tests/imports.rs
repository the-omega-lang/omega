use omega_parser::SourceModule;
use omega_parser::prelude::{ImportStmt, Item, PathAnchor};

fn import(source: &str) -> ImportStmt {
    let module = SourceModule::parse(source).expect("expected this import to parse");
    let Item::Import(import) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected an import item");
    };
    import
}

fn path_segments(import: &ImportStmt) -> Vec<String> {
    import
        .path
        .segments()
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn unprefixed_import_is_top_level() {
    let import = import("import std::io::println;");
    assert_eq!(import.path.anchor, None);
    assert_eq!(path_segments(&import), ["std", "io", "println"]);
}

#[test]
fn root_anchor_parses() {
    let import = import("import root::cmp::Ord;");
    assert_eq!(import.path.anchor, Some(PathAnchor::Root));
    assert_eq!(path_segments(&import), ["cmp", "Ord"]);
}

#[test]
fn self_anchor_parses() {
    let import = import("import self::helper;");
    assert_eq!(import.path.anchor, Some(PathAnchor::SelfModule));
    assert_eq!(path_segments(&import), ["helper"]);
}

#[test]
fn single_super_anchor_parses() {
    let import = import("import super::helper;");
    assert_eq!(import.path.anchor, Some(PathAnchor::Super(1)));
    assert_eq!(path_segments(&import), ["helper"]);
}

#[test]
fn chained_super_anchor_counts_each_occurrence() {
    let import = import("import super::super::super::helper;");
    assert_eq!(import.path.anchor, Some(PathAnchor::Super(3)));
    assert_eq!(path_segments(&import), ["helper"]);
}

#[test]
fn reveal_parses_with_every_anchor() {
    assert!(import("import reveal std::io::println;").reveal);
    assert_eq!(
        import("import reveal root::cmp::Ord;").path.anchor,
        Some(PathAnchor::Root)
    );
    assert!(import("import reveal root::cmp::Ord;").reveal);
    assert_eq!(
        import("import reveal self::helper;").path.anchor,
        Some(PathAnchor::SelfModule)
    );
    assert!(import("import reveal self::helper;").reveal);
    assert_eq!(
        import("import reveal super::helper;").path.anchor,
        Some(PathAnchor::Super(1))
    );
    assert!(import("import reveal super::helper;").reveal);
}

#[test]
fn extern_is_an_ordinary_identifier_in_import_paths() {
    // `extern` was removed as a keyword in favor of `foreign`; it now parses
    // like any other identifier, including as an import path segment.
    let module = import("import extern::std::io::println;");
    assert_eq!(path_segments(&module), ["extern", "std", "io", "println"]);
}

#[test]
fn root_self_and_super_remain_ordinary_identifiers_outside_import_anchor_position() {
    // The final path segment is an ordinary identifier position, not an
    // anchor: these spellings must still be usable there.
    let named_root = import("import config::root;");
    assert_eq!(named_root.path.anchor, None);
    assert_eq!(path_segments(&named_root), ["config", "root"]);

    let named_self = import("import config::self;");
    assert_eq!(path_segments(&named_self), ["config", "self"]);

    let named_super = import("import config::super;");
    assert_eq!(path_segments(&named_super), ["config", "super"]);
}

// Anchors are now ordinary `Path` syntax, not import-only: they must parse
// identically in type position, nested inside pointer/generic syntax, and in
// expression position.

fn parse_type(source: &str) -> omega_parser::prelude::Type {
    let wrapped = format!("x : {source};");
    let module = SourceModule::parse(&wrapped).expect("expected this type to parse");
    let Item::Declaration(decl) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected a declaration item");
    };
    decl.r#type
}

fn named_path(ty: &omega_parser::prelude::Type) -> &omega_parser::prelude::Path {
    match ty {
        omega_parser::prelude::Type::Named(path) => path,
        other => panic!("expected a named type, got {other:?}"),
    }
}

#[test]
fn anchored_ordinary_type_path_parses() {
    let ty = parse_type("self::T");
    assert_eq!(named_path(&ty).anchor, Some(PathAnchor::SelfModule));
    assert_eq!(
        named_path(&ty)
            .segments()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["T"]
    );

    let ty = parse_type("root::pkg::T");
    assert_eq!(named_path(&ty).anchor, Some(PathAnchor::Root));

    let ty = parse_type("super::super::T");
    assert_eq!(named_path(&ty).anchor, Some(PathAnchor::Super(2)));
}

#[test]
fn anchored_nested_pointer_type_path_parses() {
    let ty = parse_type("*self::T");
    let omega_parser::prelude::Type::Pointer(pointee, _) = &ty else {
        panic!("expected a pointer type, got {ty:?}");
    };
    assert_eq!(named_path(pointee).anchor, Some(PathAnchor::SelfModule));
}

#[test]
fn anchored_generic_argument_path_parses() {
    let ty = parse_type("Box<self::T>");
    let omega_parser::prelude::Type::Generic(_, args) = &ty else {
        panic!("expected a generic type, got {ty:?}");
    };
    assert_eq!(
        named_path(args[0].as_type().expect("a type argument")).anchor,
        Some(PathAnchor::SelfModule)
    );
}

#[test]
fn anchored_expression_path_parses() {
    let source = "f() => i32 { return root::pkg::CONST; }";
    let module = SourceModule::parse(source).expect("expected this function to parse");
    let Item::FunctionDefinition(func) = module.nodes.into_iter().next().unwrap().item else {
        panic!("expected a function item");
    };
    let Some(omega_parser::prelude::Statement::Return(ret)) = func
        .codeblock
        .statements
        .last()
        .map(|s| s.statement.clone())
    else {
        panic!("expected a return statement");
    };
    let omega_parser::prelude::Expression::Path(expr_path) = ret.return_value.expression else {
        panic!(
            "expected a path expression, got {:?}",
            ret.return_value.expression
        );
    };
    assert_eq!(expr_path.path.anchor, Some(PathAnchor::Root));
}

// Import trees: `as` renaming, recursive brace groups, the group-local `self`
// leaf, and subtree-scoped `reveal`. The parser keeps the written shape;
// `ImportStmt::leaves` is the flat binding view every consumer uses.

fn parse_errors(source: &str) -> Vec<String> {
    parse_errors_with_spans(source)
        .into_iter()
        .map(|(message, _)| message)
        .collect()
}

/// Each rejection as `(message, the source text it points at)`.
fn parse_errors_with_spans(source: &str) -> Vec<(String, String)> {
    match SourceModule::parse(source) {
        Ok(_) => panic!("expected this import to be rejected"),
        Err(errors) => errors
            .iter()
            .map(|e| (e.to_string(), source[e.span.start..e.span.end].to_string()))
            .collect(),
    }
}

/// Every binding an import denotes, as `(path, bound name, reveal)`.
fn leaves(source: &str) -> Vec<(Vec<String>, String, bool)> {
    import(source)
        .leaves()
        .iter()
        .map(|leaf| {
            (
                leaf.path
                    .segments()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                leaf.name.to_string(),
                leaf.reveal,
            )
        })
        .collect()
}

#[test]
fn standalone_rename_binds_only_the_new_name() {
    assert_eq!(
        leaves("import thing::Thing as ImportedThing;"),
        [(
            vec!["thing".into(), "Thing".into()],
            "ImportedThing".into(),
            false
        )]
    );
}

#[test]
fn group_flattens_to_independent_bindings_in_textual_order() {
    assert_eq!(
        leaves("import thing::{ First, Second as Two, sub::{ Third, Fourth as Four } };"),
        [
            (vec!["thing".into(), "First".into()], "First".into(), false),
            (vec!["thing".into(), "Second".into()], "Two".into(), false),
            (
                vec!["thing".into(), "sub".into(), "Third".into()],
                "Third".into(),
                false
            ),
            (
                vec!["thing".into(), "sub".into(), "Fourth".into()],
                "Four".into(),
                false
            ),
        ]
    );
}

#[test]
fn multi_segment_group_entry_extends_the_prefix() {
    assert_eq!(
        leaves("import thing::{ deep::nested::Item };"),
        [(
            vec![
                "thing".into(),
                "deep".into(),
                "nested".into(),
                "Item".into()
            ],
            "Item".into(),
            false
        )]
    );
}

#[test]
fn self_leaf_binds_the_enclosing_prefix() {
    assert_eq!(
        leaves("import thing::{ self, Thing };"),
        [
            (vec!["thing".into()], "thing".into(), false),
            (vec!["thing".into(), "Thing".into()], "Thing".into(), false),
        ]
    );
    assert_eq!(
        leaves("import thing::{ self as TheModule };"),
        [(vec!["thing".into()], "TheModule".into(), false)]
    );
    assert_eq!(
        leaves("import thing::{ sub::{ self, Item } };"),
        [
            (vec!["thing".into(), "sub".into()], "sub".into(), false),
            (
                vec!["thing".into(), "sub".into(), "Item".into()],
                "Item".into(),
                false
            ),
        ]
    );
}

#[test]
fn reveal_is_inherited_by_every_descendant_leaf() {
    assert_eq!(
        leaves("import reveal abc::{ A, sub::{ B, C } };"),
        [
            (vec!["abc".into(), "A".into()], "A".into(), true),
            (
                vec!["abc".into(), "sub".into(), "B".into()],
                "B".into(),
                true
            ),
            (
                vec!["abc".into(), "sub".into(), "C".into()],
                "C".into(),
                true
            ),
        ]
    );
}

#[test]
fn reveal_on_a_subtree_stops_at_that_subtree() {
    assert_eq!(
        leaves("import abc::{ reveal A, B };"),
        [
            (vec!["abc".into(), "A".into()], "A".into(), true),
            (vec!["abc".into(), "B".into()], "B".into(), false),
        ]
    );
    assert_eq!(
        leaves("import abc::{ reveal sub::{ A, B }, C };"),
        [
            (
                vec!["abc".into(), "sub".into(), "A".into()],
                "A".into(),
                true
            ),
            (
                vec!["abc".into(), "sub".into(), "B".into()],
                "B".into(),
                true
            ),
            (vec!["abc".into(), "C".into()], "C".into(), false),
        ]
    );
}

#[test]
fn redundant_nested_reveal_yields_one_revealed_leaf() {
    assert_eq!(
        leaves("import reveal thing::{ reveal A };"),
        [(vec!["thing".into(), "A".into()], "A".into(), true)]
    );
}

#[test]
fn reveal_self_follows_the_same_inheritance_rule() {
    assert_eq!(
        leaves("import thing::{ reveal self, Other };"),
        [
            (vec!["thing".into()], "thing".into(), true),
            (vec!["thing".into(), "Other".into()], "Other".into(), false),
        ]
    );
}

#[test]
fn group_prefixes_accept_every_anchor() {
    let anchored = import("import self::module::{ A };");
    assert_eq!(anchored.path.anchor, Some(PathAnchor::SelfModule));
    assert_eq!(
        leaves("import self::module::{ A };"),
        [(vec!["module".into(), "A".into()], "A".into(), false)]
    );

    assert_eq!(
        import("import root::module::{ A };").path.anchor,
        Some(PathAnchor::Root)
    );
    assert_eq!(
        import("import super::super::module::{ A };").path.anchor,
        Some(PathAnchor::Super(2))
    );
}

#[test]
fn trailing_commas_are_allowed_in_groups() {
    assert_eq!(
        leaves("import thing::{ A, sub::{ B, }, };"),
        [
            (vec!["thing".into(), "A".into()], "A".into(), false),
            (
                vec!["thing".into(), "sub".into(), "B".into()],
                "B".into(),
                false
            ),
        ]
    );
}

#[test]
fn as_remains_an_ordinary_identifier_outside_the_connector_position() {
    assert_eq!(
        leaves("import as::thing::as;"),
        [(
            vec!["as".into(), "thing".into(), "as".into()],
            "as".into(),
            false
        )]
    );
    assert_eq!(
        leaves("import thing::{ as };"),
        [(vec!["thing".into(), "as".into()], "as".into(), false)]
    );
    assert_eq!(
        leaves("import thing::Thing as as;"),
        [(vec!["thing".into(), "Thing".into()], "as".into(), false)]
    );
    SourceModule::parse("as() => i32 { 1 }").expect("`as` is still an item name");
}

#[test]
fn leaf_spans_point_at_the_group_entry() {
    let source = "import thing::{ First, Second as Two };";
    let import = import(source);
    let spans: Vec<&str> = import
        .leaves()
        .iter()
        .map(|leaf| &source[leaf.span.start..leaf.span.end])
        .collect();
    assert_eq!(spans, ["First", "Second as Two"]);
}

#[test]
fn ungrouped_import_leaf_spans_the_whole_statement() {
    let source = "import thing::Thing;";
    let import = import(source);
    let leaf = &import.leaves()[0];
    assert_eq!(&source[leaf.span.start..leaf.span.end], source);
}

#[test]
fn empty_group_is_rejected() {
    assert!(
        parse_errors("import thing::{};")
            .iter()
            .any(|e| e.contains("at least one name"))
    );
}

#[test]
fn renaming_a_group_prefix_is_rejected() {
    assert!(
        parse_errors("import thing::{ sub as other::{ A } };")
            .iter()
            .any(|e| e.contains("only a complete import binding"))
    );
}

#[test]
fn renaming_a_group_prefix_points_at_the_as() {
    for source in [
        "import foo as bar::{ Thing };",
        "import thing::{ sub as other::{ A } };",
    ] {
        let (_, spelling) = parse_errors_with_spans(source)
            .into_iter()
            .find(|(message, _)| message.contains("only a complete import binding"))
            .expect("expected a prefix-rename rejection");
        assert_eq!(spelling, "as", "in {source:?}");
    }
}

#[test]
fn non_terminal_self_is_rejected() {
    for source in [
        "import thing::{ self::Item };",
        "import thing::{ self::{ Item } };",
    ] {
        assert!(
            parse_errors(source)
                .iter()
                .any(|e| e.contains("cannot be extended")),
            "expected a non-terminal `self` diagnostic for {source}"
        );
    }
}

#[test]
fn anchor_only_group_prefix_is_rejected() {
    for source in [
        "import self::{ thing };",
        "import root::{ thing };",
        "import super::{ thing };",
    ] {
        assert!(
            parse_errors(source)
                .iter()
                .any(|e| e.contains("identifier")),
            "expected an anchor-only group prefix to be rejected for {source}"
        );
    }
}
