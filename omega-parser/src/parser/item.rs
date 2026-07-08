use crate::ast::identifier::Ident;
use crate::ast::statement::{
    Item, ItemNode, declaration::DeclarationStmt,
    function_definition::FunctionDefinitionStmt, import::ImportStmt, r#struct::StructStmt,
};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::expression::parse_codeblock;
use crate::parser::macro_syntax::{parse_macro_definition, parse_macro_invocation};
use crate::parser::statement::{parse_declaration, parse_extern_declaration};
use crate::parser::{Parser, parse_path, recovery};

/// Parses a whole source file's top-level items, recovering after each
/// failed one (see `recovery::synchronize_to_item_boundary`) so a single
/// mistake reports one error and the rest of the file still gets checked,
/// rather than aborting on the first problem.
pub fn parse_source_module(p: &mut Parser) -> Vec<ItemNode> {
    let mut nodes = Vec::new();
    while !p.is_eof() {
        match parse_item(p) {
            Some(node) => nodes.push(node),
            None => recovery::synchronize_to_item_boundary(p),
        }
    }
    nodes
}

pub fn parse_item(p: &mut Parser) -> Option<ItemNode> {
    let start = p.peek_span();
    let item = match p.peek() {
        TokenKind::Extern => {
            let decl = parse_extern_declaration(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::ExternDeclaration(decl)
        }
        TokenKind::Import => {
            p.advance();
            let path = parse_path(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::Import(ImportStmt { path })
        }
        TokenKind::Struct => Item::Struct(parse_struct_def(p)?),
        TokenKind::Macro => Item::MacroDefinition(parse_macro_definition(p)?),
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Bang) => {
            let inv = parse_macro_invocation(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::MacroInvocation(inv)
        }
        TokenKind::Ident(_) => parse_declaration_or_function_definition(p)?,
        _ => {
            p.error(ParseErrorKind::Expected { expected: "a top-level item", found: p.peek().describe() });
            return None;
        }
    };
    let span = start.to(p.last_span());
    Some(ItemNode { item, span })
}

/// A leading identifier could start either a plain `Declaration`
/// (`ident: Type;`) or a `FunctionDefinition` (`ident<generics>(params) =>
/// Type { ... }`) -- disambiguated with a single-token lookahead, no
/// backtracking needed: only a function definition can have `<generics>` or
/// `(params)` at all in this position, so seeing `<` or `(` immediately
/// after the name is already conclusive on its own, without needing to look
/// *past* the (possibly absent, possibly multi-token) generics list first.
fn parse_declaration_or_function_definition(p: &mut Parser) -> Option<Item> {
    match p.peek_at(1) {
        TokenKind::Lt | TokenKind::LParen => Some(Item::FunctionDefinition(parse_function_definition(p)?)),
        _ => {
            let decl = parse_declaration(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Some(Item::Declaration(decl))
        }
    }
}

/// `name<T, U, ...>(params) => ReturnType { body }` -- shared verbatim
/// between a top-level function definition and a struct method (see
/// `parse_struct_def`), exactly like the old grammar's single
/// `FunctionDefinitionStmt::parser` was.
pub fn parse_function_definition(p: &mut Parser) -> Option<FunctionDefinitionStmt> {
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LParen, "'('");
    let (is_member_function, params) = parse_param_list(p);
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type = crate::parser::r#type::parse_type(p)?;
    let codeblock = parse_codeblock(p)?;
    Some(FunctionDefinitionStmt { ident, generics, is_member_function, params, return_type, codeblock })
}

/// `<T, U, ...>` -- optional, at least one name if present.
fn parse_optional_generics(p: &mut Parser) -> Option<Vec<Ident>> {
    if !p.eat(&TokenKind::Lt) {
        return Some(Vec::new());
    }
    let mut generics = vec![p.expect_ident()?];
    while p.eat(&TokenKind::Comma) {
        generics.push(p.expect_ident()?);
    }
    p.expect(&TokenKind::Gt, "'>'");
    Some(generics)
}

/// `self` (optionally followed by `, ident: Type, ...`), or just
/// `ident: Type, ...` -- `self` is a contextual keyword here (see
/// `lexer::TokenKind`'s doc comment), checked by comparing an already-lexed
/// `Ident`'s text.
fn parse_param_list(p: &mut Parser) -> (bool, Vec<DeclarationStmt>) {
    if let TokenKind::Ident(name) = p.peek()
        && name == "self"
    {
        p.advance();
        let rest = if p.eat(&TokenKind::Comma) { parse_declaration_list(p) } else { Vec::new() };
        return (true, rest);
    }
    (false, parse_declaration_list(p))
}

/// Zero or more `ident: Type` pairs, comma-separated -- a comma is only
/// consumed if another declaration actually follows, so a trailing comma
/// before `)`/`}` is left unconsumed (a real parse error at the caller,
/// matching the old grammar's plain `separated_by`, which doesn't tolerate
/// one either) rather than silently swallowed.
fn parse_declaration_list(p: &mut Parser) -> Vec<DeclarationStmt> {
    let mut decls = Vec::new();
    if !matches!(p.peek(), TokenKind::Ident(_)) {
        return decls;
    }
    while let Some(decl) = parse_declaration(p) {
        decls.push(decl);
        if matches!(p.peek(), TokenKind::Comma) && matches!(p.peek_at(1), TokenKind::Ident(_)) {
            p.advance();
        } else {
            break;
        }
    }
    decls
}

/// `struct Name<T, ...> { field: Type; ... method(...) => T { ... } ... }`
/// -- fields and methods are structurally two separate phases, fields
/// always first (matching the old grammar's `declarations_parser.repeated()`
/// *then* `functions_parser.repeated()`, not an interleaved single loop):
/// once the field-shaped lookahead (`Ident` + `:`) stops matching, the
/// struct body is assumed to be all methods from there on. Shared between
/// root-item position (`Item::Struct`) and nested statement
/// position (`Statement::Struct`, see `parser::statement`) exactly like the
/// old grammar's single `StructStmt::parser` was.
pub fn parse_struct_def(p: &mut Parser) -> Option<StructStmt> {
    p.expect(&TokenKind::Struct, "'struct'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    let mut fields = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) && matches!(p.peek_at(1), TokenKind::Colon) {
        match parse_declaration(p) {
            Some(decl) => {
                fields.push(decl);
                p.expect_terminator(&TokenKind::Semi, "';'");
            }
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }

    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        match parse_function_definition(p) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }

    p.expect(&TokenKind::RBrace, "'}'");
    Some(StructStmt { ident, generics, fields, functions })
}
