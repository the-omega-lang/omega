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
    import.path.segments().iter().map(ToString::to_string).collect()
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
    assert_eq!(
        path_segments(&module),
        ["extern", "std", "io", "println"]
    );
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
    assert_eq!(named_path(&ty).segments().iter().map(ToString::to_string).collect::<Vec<_>>(), ["T"]);

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
    assert_eq!(named_path(&args[0]).anchor, Some(PathAnchor::SelfModule));
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
