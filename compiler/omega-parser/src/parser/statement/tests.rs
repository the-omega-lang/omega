use crate::SourceModule;
use crate::ast::item::Item;
use crate::ast::statement::{AsmDescriptorKind, Statement};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;

fn body_statements(source: &str) -> Vec<Statement> {
    let module = SourceModule::parse(source).expect("source must parse");
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("first item must be a function");
    };
    f.codeblock
        .statements
        .iter()
        .map(|s| s.statement.clone())
        .collect()
}

fn errors(source: &str) -> Vec<ParseErrorKind> {
    SourceModule::parse(source)
        .err()
        .expect("expected this source to be rejected")
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn asm_with_all_descriptor_forms_parses() {
    let stmts = body_statements(
        "f() => void { asm(reg(x), reg(y, \"rcx\"), const(SIZE), clobber(\"rax\")) => { nop } }",
    );
    let Statement::InlineAsm(asm) = &stmts[0] else {
        panic!("expected an inline-asm statement, got {:?}", stmts[0]);
    };
    assert_eq!(asm.descriptors.len(), 4);
    assert!(matches!(
        asm.descriptors[0].kind,
        AsmDescriptorKind::Reg { physical: None, .. }
    ));
    let AsmDescriptorKind::Reg {
        physical: Some(ref reg),
        ..
    } = asm.descriptors[1].kind
    else {
        panic!("expected a physical-register reg descriptor");
    };
    assert_eq!(reg, "rcx");
    let AsmDescriptorKind::Const { ref name, .. } = asm.descriptors[2].kind else {
        panic!("expected a const descriptor");
    };
    assert_eq!(name.as_ref(), "SIZE");
    let AsmDescriptorKind::Clobber { ref register } = asm.descriptors[3].kind else {
        panic!("expected a clobber descriptor");
    };
    assert_eq!(register, "rax");
}

#[test]
fn asm_needs_no_trailing_semicolon_like_other_block_statements() {
    let stmts = body_statements("f() => void { asm() => { nop } g(); } ");
    assert_eq!(stmts.len(), 2);
    assert!(matches!(stmts[0], Statement::InlineAsm(_)));
}

#[test]
fn asm_body_is_captured_byte_for_byte() {
    let stmts = body_statements(
        "f() => void { asm(reg(x)) => {\n    # not an Omega comment: mov $x, 1 ; done\n    mov $x, 1\n} }",
    );
    let Statement::InlineAsm(asm) = &stmts[0] else {
        panic!("expected an inline-asm statement");
    };
    assert!(
        asm.body
            .contains("# not an Omega comment: mov $x, 1 ; done")
    );
    assert!(asm.body.contains("mov $x, 1"));
}

#[test]
fn asm_body_preserves_unknown_target_punctuation_and_dollar_markers() {
    let stmts = body_statements(
        "f() => void { asm(reg(x), reg(y)) => { add $y, 22; mov [$x], $$10 // not omega // } }",
    );
    let Statement::InlineAsm(asm) = &stmts[0] else {
        panic!("expected an inline-asm statement");
    };
    assert_eq!(
        asm.body.trim(),
        "add $y, 22; mov [$x], $$10 // not omega //"
    );
}

#[test]
fn asm_body_balances_nested_braces() {
    let stmts = body_statements("f() => void { asm() => { { nested } ${also_nested} } g(); } ");
    let Statement::InlineAsm(asm) = &stmts[0] else {
        panic!("expected an inline-asm statement");
    };
    assert_eq!(asm.body.trim(), "{ nested } ${also_nested}");
    assert!(matches!(stmts[1], Statement::Expression(_)));
}

#[test]
fn unterminated_asm_body_is_a_dedicated_error() {
    assert!(matches!(
        errors("f() => void { asm() => { nop ").as_slice(),
        [ParseErrorKind::UnterminatedAsmBody, ..]
    ));
}

#[test]
fn asm_without_fat_arrow_body_is_an_ordinary_call() {
    // `asm(...)` not followed by `=> {` must remain an ordinary call
    // expression -- `asm` stays usable as a plain identifier/function name.
    let module = SourceModule::parse("asm(a: i32) => i32 { a }\nf() => void { asm(1); }")
        .expect("source must parse");
    let Item::FunctionDefinition(f) = &module.nodes[1].item else {
        panic!("second item must be a function");
    };
    assert!(matches!(
        f.codeblock.statements[0].statement,
        Statement::Expression(_)
    ));
}

#[test]
fn asm_body_does_not_get_ordinary_tokenization() {
    let tokens = crate::lexer::tokenize("asm() => { \"unterminated because it is not omega }").0;
    // The raw body absorbs the stray quote as ordinary text instead of the
    // lexer trying (and failing) to scan it as an Omega string literal.
    assert!(
        tokens.iter().any(
            |t| matches!(&t.kind, TokenKind::AsmBody(text) if text.contains("\"unterminated"))
        )
    );
}
