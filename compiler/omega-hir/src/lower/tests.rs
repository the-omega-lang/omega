use super::lower_module;
use crate::hir::{HirAsmDescriptorKind, HirExpr, HirItem, HirStmt};
use crate::ids::ModuleId;
use omega_parser::SourceModule;
use omega_parser::prelude::Type;

fn lower(source: &str) -> crate::hir::HirModule {
    let ast = SourceModule::parse(source).expect("test source must parse");
    lower_module(ModuleId(0), &ast)
}

#[test]
fn declaration_hir_span_uses_the_declaration_not_its_initializer() {
    let source = "x: i32 = 1;";
    let hir = lower(source);
    let HirItem::DeclarationWithInit { decl, .. } = &hir.items[0] else {
        panic!("expected initialized declaration");
    };

    assert_eq!(&source[decl.span.start..decl.span.end], "x: i32");
}

#[test]
fn member_function_hir_span_is_the_function_not_the_enclosing_type() {
    let source = "struct S { f() => void {} }";
    let hir = lower(source);
    let HirItem::Struct(def) = &hir.items[0] else {
        panic!("expected struct");
    };
    let function = &def.functions[0];

    assert_eq!(
        &source[function.span.start..function.span.end],
        "f() => void {}"
    );
}

#[test]
fn gap_function_hir_span_is_its_signature() {
    let source = "gap G { f() => void; }";
    let hir = lower(source);
    let HirItem::Gap(def) = &hir.items[0] else {
        panic!("expected gap");
    };
    let function = &def.functions[0];

    assert_eq!(
        &source[function.span.start..function.span.end],
        "f() => void"
    );
}

#[test]
fn inline_asm_lowers_descriptors_and_raw_body_in_source_order() {
    let source = "f() => void { asm(reg(x), comp(SIZE), clobber(\"rax\")) => { mov $x, $SIZE } }";
    let hir = lower(source);
    let HirItem::FunctionDefinition(f) = &hir.items[0] else {
        panic!("expected a function");
    };
    let HirStmt::InlineAsm(asm) = &f.body.as_ref().unwrap().stmts[0] else {
        panic!("expected an inline-asm statement");
    };
    assert_eq!(asm.descriptors.len(), 3);
    assert!(matches!(
        asm.descriptors[0].kind,
        HirAsmDescriptorKind::Reg { .. }
    ));
    assert!(matches!(
        asm.descriptors[1].kind,
        HirAsmDescriptorKind::Comp { .. }
    ));
    assert!(matches!(
        asm.descriptors[2].kind,
        HirAsmDescriptorKind::Clobber { .. }
    ));
    assert_eq!(asm.body.trim(), "mov $x, $SIZE");
    assert_eq!(&source[asm.span.start..asm.span.start + 3], "asm");
}

#[test]
fn foreign_block_convention_applies_only_to_direct_function_entries() {
    let source = "foreign(c) { fp : (x: i32) => void; direct(x: i32) => void; }";
    let hir = lower(source);

    let HirItem::ForeignBinding(binding) = &hir.items[0] else {
        panic!("expected a foreign binding for `fp`");
    };
    let Type::Function(fn_type) = &binding.r#type else {
        panic!("expected `fp` to keep its explicit function type");
    };
    assert_eq!(
        fn_type.convention, None,
        "an explicit `: Type` entry must keep its own type's convention, not inherit the block's"
    );

    let HirItem::ForeignFunction(direct) = &hir.items[1] else {
        panic!("expected a foreign function for `direct`");
    };
    assert_eq!(
        direct.convention.as_ref().map(|c| c.name.0.as_str()),
        Some("c"),
        "a direct block entry must inherit the block's convention"
    );
}

#[test]
fn receiver_and_its_mut_shadow_point_at_the_self_token() {
    let source = "struct S { f(*mut self) => void {} g(mut self) => void {} }";
    let hir = lower(source);
    let HirItem::Struct(def) = &hir.items[0] else {
        panic!("expected struct");
    };
    for function in &def.functions {
        let receiver = &function.params[0];
        assert_eq!(
            &source[receiver.name_span.start..receiver.name_span.end],
            "self"
        );
        assert_eq!(&source[receiver.span.start..receiver.span.end], "self");
    }
    let shadow = &def.functions[1].body.as_ref().expect("body").stmts[0];
    let HirStmt::WalrusDeclaration(shadow) = shadow else {
        panic!("expected the `mut self` shadow");
    };
    assert_eq!(&source[shadow.span.start..shadow.span.end], "self");
}

#[test]
fn foreign_binding_mut_span_survives_lowering() {
    let source = "foreign mut counter : i32; foreign(c) { mut other : i32; }";
    let hir = lower(source);
    assert_eq!(hir.items.len(), 2);
    for item in &hir.items {
        let HirItem::ForeignBinding(binding) = item else {
            panic!("expected a foreign binding");
        };
        let span = binding.mut_span.expect("expected a mut span");
        assert_eq!(&source[span.start..span.end], "mut");
    }
}

#[test]
fn import_tree_lowers_to_one_flat_binding_per_leaf() {
    let source = "@suppress(unused_import)\n\
                  import reveal thing::{ self as Mod, First, sub::{ Second as Two } };";
    let hir = lower(source);
    let bindings: Vec<(Vec<String>, String, bool, usize, &str)> = hir
        .items
        .iter()
        .map(|item| {
            let HirItem::Import(import) = item else {
                panic!("expected every lowered item to be an import");
            };
            (
                import
                    .path
                    .segments()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                import.name.to_string(),
                import.reveal,
                import.annotations.len(),
                &source[import.span.start..import.span.end],
            )
        })
        .collect();

    assert_eq!(
        bindings,
        [
            (vec!["thing".into()], "Mod".into(), true, 1, "self as Mod"),
            (
                vec!["thing".into(), "First".into()],
                "First".into(),
                true,
                1,
                "First"
            ),
            (
                vec!["thing".into(), "sub".into(), "Second".into()],
                "Two".into(),
                true,
                1,
                "Second as Two"
            ),
        ]
    );
}

#[test]
fn each_import_leaf_gets_its_own_hir_id() {
    let hir = lower("import thing::{ A, B };");
    let ids: Vec<_> = hir
        .items
        .iter()
        .map(|item| {
            let HirItem::Import(import) = item else {
                panic!("expected an import");
            };
            import.id
        })
        .collect();

    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn reveal_prefixes_fold_into_the_place_they_wrap() {
    let source = "f() => void { a := reveal reveal v.secret; b := reveal g(); }";
    let hir = lower(source);
    let HirItem::FunctionDefinition(f) = &hir.items[0] else {
        panic!("expected a function");
    };
    let stmts = &f.body.as_ref().unwrap().stmts;
    let text = |span: omega_parser::prelude::Span| &source[span.start..span.end];

    let HirStmt::WalrusDeclaration(revealed_place) = &stmts[0] else {
        panic!("expected a declaration");
    };
    let HirExpr::Place(place) = &revealed_place.value.expr else {
        panic!("a revealed place lowers to the place itself");
    };
    assert_eq!(text(revealed_place.value.span), "v.secret");
    let spans: Vec<&str> = place
        .reveals
        .iter()
        .map(|reveal| text(reveal.span))
        .collect();
    assert_eq!(spans, ["reveal reveal v.secret", "reveal v.secret"]);

    let HirStmt::WalrusDeclaration(revealed_call) = &stmts[1] else {
        panic!("expected a declaration");
    };
    assert!(matches!(revealed_call.value.expr, HirExpr::Reveal(_)));
}
