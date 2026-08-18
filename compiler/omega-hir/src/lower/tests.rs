use super::lower_module;
use crate::hir::HirItem;
use crate::ids::ModuleId;
use omega_parser::SourceModule;

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
