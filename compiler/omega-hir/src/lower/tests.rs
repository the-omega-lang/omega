use super::lower_module;
use crate::hir::{HirAsmDescriptorKind, HirItem, HirStmt};
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

#[test]
fn inline_asm_lowers_descriptors_and_raw_body_in_source_order() {
    let source = "f() => void { asm(reg(x), const(SIZE), clobber(\"rax\")) => { mov $x, $SIZE } }";
    let hir = lower(source);
    let HirItem::FunctionDefinition(f) = &hir.items[0] else {
        panic!("expected a function");
    };
    let HirStmt::InlineAsm(asm) = &f.body.stmts[0] else {
        panic!("expected an inline-asm statement");
    };
    assert_eq!(asm.descriptors.len(), 3);
    assert!(matches!(
        asm.descriptors[0].kind,
        HirAsmDescriptorKind::Reg { .. }
    ));
    assert!(matches!(
        asm.descriptors[1].kind,
        HirAsmDescriptorKind::Const { .. }
    ));
    assert!(matches!(
        asm.descriptors[2].kind,
        HirAsmDescriptorKind::Clobber { .. }
    ));
    assert_eq!(asm.body.trim(), "mov $x, $SIZE");
    assert_eq!(&source[asm.span.start..asm.span.start + 3], "asm");
}
