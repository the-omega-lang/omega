use omega_parser::SourceModule;
use omega_parser::prelude::{ImportRoot, ImportStmt, Item};

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
    assert_eq!(import.root, ImportRoot::TopLevel);
    assert_eq!(path_segments(&import), ["std", "io", "println"]);
}

#[test]
fn root_anchor_parses() {
    let import = import("import root::cmp::Ord;");
    assert_eq!(import.root, ImportRoot::Root);
    assert_eq!(path_segments(&import), ["cmp", "Ord"]);
}

#[test]
fn self_anchor_parses() {
    let import = import("import self::helper;");
    assert_eq!(import.root, ImportRoot::SelfModule);
    assert_eq!(path_segments(&import), ["helper"]);
}

#[test]
fn single_super_anchor_parses() {
    let import = import("import super::helper;");
    assert_eq!(import.root, ImportRoot::Super(1));
    assert_eq!(path_segments(&import), ["helper"]);
}

#[test]
fn chained_super_anchor_counts_each_occurrence() {
    let import = import("import super::super::super::helper;");
    assert_eq!(import.root, ImportRoot::Super(3));
    assert_eq!(path_segments(&import), ["helper"]);
}

#[test]
fn reveal_parses_with_every_anchor() {
    assert!(import("import reveal std::io::println;").reveal);
    assert_eq!(import("import reveal root::cmp::Ord;").root, ImportRoot::Root);
    assert!(import("import reveal root::cmp::Ord;").reveal);
    assert_eq!(
        import("import reveal self::helper;").root,
        ImportRoot::SelfModule
    );
    assert!(import("import reveal self::helper;").reveal);
    assert_eq!(
        import("import reveal super::helper;").root,
        ImportRoot::Super(1)
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
    assert_eq!(named_root.root, ImportRoot::TopLevel);
    assert_eq!(path_segments(&named_root), ["config", "root"]);

    let named_self = import("import config::self;");
    assert_eq!(path_segments(&named_self), ["config", "self"]);

    let named_super = import("import config::super;");
    assert_eq!(path_segments(&named_super), ["config", "super"]);
}
