use omega_parser::SourceModule;
use omega_parser::prelude::{Item, ParseErrorKind, RawConvention, Type};

fn parse_item(source: &str) -> Item {
    let module = SourceModule::parse(source).expect("expected this item to parse");
    module.nodes.into_iter().next().unwrap().item
}

fn convention_name(convention: &Option<RawConvention>) -> Option<&str> {
    convention.as_ref().map(|c| c.name.0.as_str())
}

#[test]
fn foreign_binding_parses() {
    let item = parse_item("foreign errno : i32;");
    let Item::ForeignBinding(binding) = item else {
        panic!("expected a foreign binding item, got {item:?}");
    };
    assert_eq!(binding.ident.0, "errno");
    assert!(matches!(binding.r#type, Type::Named(_)));
}

#[test]
fn direct_foreign_function_declaration_parses() {
    let item = parse_item("foreign(c) puts(s: *u8) => i32;");
    let Item::ForeignFunction(func) = item else {
        panic!("expected a foreign function item, got {item:?}");
    };
    assert_eq!(func.ident.0, "puts");
    assert_eq!(convention_name(&func.convention), Some("c"));
    assert_eq!(func.params.len(), 1);
    assert!(!func.is_variadic);
    assert!(func.body.is_none());
}

#[test]
fn direct_foreign_function_definition_with_body_parses() {
    let item = parse_item("foreign(sysv64) add(a: i32, b: i32) => i32 { return a + b; }");
    let Item::ForeignFunction(func) = item else {
        panic!("expected a foreign function item, got {item:?}");
    };
    assert_eq!(func.ident.0, "add");
    assert_eq!(convention_name(&func.convention), Some("sysv64"));
    assert!(func.body.is_some());
}

#[test]
fn foreign_block_groups_multiple_items() {
    let item = parse_item(
        r#"
        foreign(c) {
            puts(s: *u8) => i32;
            errno : i32;
        }
        "#,
    );
    let Item::ForeignBlock(block) = item else {
        panic!("expected a foreign block item, got {item:?}");
    };
    assert_eq!(convention_name(&block.convention), Some("c"));
    assert_eq!(block.entries.len(), 2);
    assert!(matches!(
        block.entries[0],
        omega_parser::prelude::ForeignBlockEntry::Function(_)
    ));
    assert!(matches!(
        block.entries[1],
        omega_parser::prelude::ForeignBlockEntry::Binding(_)
    ));
}

#[test]
fn foreign_convention_type_parses_as_variable_type() {
    let module = SourceModule::parse("handler : foreign(c) (code: i32) => i32;")
        .expect("expected this declaration to parse");
    let Item::Declaration(decl) = &module.nodes[0].item else {
        panic!("expected a declaration item, got {:?}", module.nodes[0].item);
    };
    let Type::Function(function_type) = &decl.r#type else {
        panic!("expected a function type, got {:?}", decl.r#type);
    };
    assert_eq!(convention_name(&function_type.convention), Some("c"));
}

#[test]
fn foreign_convention_directly_on_binding_is_rejected() {
    let errors = SourceModule::parse("foreign(c) errno : i32;").unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e.kind, ParseErrorKind::ForeignConventionOnBinding)));
}

#[test]
fn nested_foreign_blocks_are_rejected() {
    let errors = SourceModule::parse(
        r#"
        foreign(c) {
            foreign(sysv64) {
                puts(s: *u8) => i32;
            }
        }
        "#,
    )
    .unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e.kind, ParseErrorKind::NestedForeignBlock)));
}

#[test]
fn malformed_convention_token_is_rejected() {
    let errors = SourceModule::parse("foreign(123) bad(s: *u8) => i32;").unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e.kind, ParseErrorKind::Expected { .. })));
}

#[test]
fn extern_is_now_an_ordinary_identifier() {
    let module = SourceModule::parse("extern := 1;").expect("`extern` must stay usable as an identifier");
    assert!(matches!(module.nodes[0].item, Item::Walrus(_)));
}
