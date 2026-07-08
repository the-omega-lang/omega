use crate::ast::expression::Expression;
use crate::ast::statement::{
    Statement, StatementNode, declaration::DeclarationStmt, defer::DeferStmt,
    extern_declaration::ExternDeclarationStmt, for_stmt::ForStmt, r#return::ReturnStmt,
    walrus::WalrusStmt, while_stmt::WhileStmt,
};
use crate::lexer::TokenKind;
use crate::parser::expression::{parse_codeblock, parse_expression};
use crate::parser::{Parser, recovery};

/// One statement, function-body scope. A deliberate cleanup from the old
/// grammar's `terminal`/`nonterminal` *group* split (which needed
/// `DeferStmt`'s own special-cased "try a block body directly, bypassing
/// the general statement grammar" carve-out, since a bare `{ ... }`
/// statement inconsistently required a trailing `;` while `if`/`while`/
/// `for` didn't): every statement's *content* parses through one dispatch
/// (`parse_statement_content`), and whether a trailing `;` is required is
/// decided *after the fact*, purely by checking whether what was actually
/// parsed is block-shaped -- not by which grammar production matched. This
/// is what lets `if`/a bare `{ ... }` fall through to the plain "parse an
/// expression" case below with no special-casing at all: `if`/`Codeblock`
/// are already ordinary `Expression` primaries (see `parser::expression`),
/// and the block-shaped check after parsing already recognizes them
/// correctly by their *outermost* shape -- `{ f(); } - g()` (outermost
/// shape `BinaryOp`) still requires `;`, exactly like today, while a bare
/// `{ f(); }` (outermost shape `Codeblock`) doesn't need one -- a pure
/// postprocessing check on "what did we just parse," incapable of changing
/// what any input parses *as*, only whether a trailing `;` is subsequently
/// required.
///
/// `struct`/`while`/`for` still get dedicated dispatch (they aren't
/// `Expression` variants at all, so the generic expression fallback could
/// never reach them), and are unconditionally block-shaped by construction.
/// `defer`'s own body is just "parse one statement's *content*" recursively
/// -- no special-casing needed there either, since `defer` simply inherits
/// its wrapped statement's block-shaped-ness and terminator handling stays
/// the sole responsibility of the outer `parse_statement`, called exactly
/// once per statement (splitting content-parsing from terminator-consuming
/// like this is what avoids `defer foo();` otherwise having its `;`
/// consumed twice -- once by a naive recursive `parse_statement` call for
/// the inner body, and again by `defer`'s own wrapping).
pub fn parse_statement(p: &mut Parser) -> Option<StatementNode> {
    let start = p.peek_span();
    let (statement, block_shaped) = parse_statement_content(p)?;
    if block_shaped {
        p.eat(&TokenKind::Semi);
    } else if !p.expect(&TokenKind::Semi, "';'") {
        return None;
    }
    let span = start.to(p.last_span());
    Some(StatementNode { statement, span })
}

/// Parses one statement's content and reports whether it's block-shaped --
/// `parse_statement` is the only caller that ever consumes a terminator for
/// it; `defer`'s own body recurses here directly (not into `parse_statement`)
/// specifically to avoid double-consuming a terminator (see this module's
/// top doc comment).
fn parse_statement_content(p: &mut Parser) -> Option<(Statement, bool)> {
    match p.peek() {
        TokenKind::Struct => Some((Statement::Struct(crate::parser::item::parse_struct_def(p)?), true)),
        TokenKind::While => Some((Statement::While(parse_while(p)?), true)),
        TokenKind::For => Some((Statement::For(Box::new(parse_for(p)?)), true)),
        TokenKind::Defer => {
            p.advance(); // 'defer'
            let (inner, block_shaped) = parse_statement_content(p)?;
            Some((Statement::Defer(DeferStmt { body: Box::new(inner) }), block_shaped))
        }
        TokenKind::Extern => Some((Statement::ExternDeclaration(parse_extern_declaration(p)?), false)),
        TokenKind::Return => Some((Statement::Return(parse_return(p)?), false)),
        TokenKind::Break => {
            p.advance();
            Some((Statement::Break, false))
        }
        TokenKind::Continue => {
            p.advance();
            Some((Statement::Continue, false))
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::ColonEq) => {
            Some((Statement::Walrus(parse_walrus(p)?), false))
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Colon) => {
            let decl = parse_declaration(p)?;
            if p.eat(&TokenKind::Eq) {
                let value = parse_expression(p)?;
                Some((Statement::DeclarationWithInit(decl, value), false))
            } else {
                Some((Statement::Declaration(decl), false))
            }
        }
        _ => {
            let expr = parse_expression(p)?;
            let block_shaped = matches!(expr.expression, Expression::Codeblock(_) | Expression::If(_));
            Some((Statement::Expression(expr), block_shaped))
        }
    }
}

/// `ident : Type` -- shared by declarations (function-body and struct-field
/// position), and by the leading name of a function/struct's own parameter/
/// field list.
pub fn parse_declaration(p: &mut Parser) -> Option<DeclarationStmt> {
    let ident = p.expect_ident()?;
    p.expect(&TokenKind::Colon, "':'");
    let r#type = crate::parser::r#type::parse_type(p)?;
    Some(DeclarationStmt { ident, r#type })
}

pub fn parse_extern_declaration(p: &mut Parser) -> Option<ExternDeclarationStmt> {
    p.expect(&TokenKind::Extern, "'extern'");
    let decl = parse_declaration(p)?;
    Some(ExternDeclarationStmt { ident: decl.ident, r#type: decl.r#type })
}

fn parse_return(p: &mut Parser) -> Option<ReturnStmt> {
    p.expect(&TokenKind::Return, "'return'");
    let return_value = parse_expression(p)?;
    Some(ReturnStmt { return_value })
}

fn parse_walrus(p: &mut Parser) -> Option<WalrusStmt> {
    let ident = p.expect_ident()?;
    p.expect(&TokenKind::ColonEq, "':='");
    let value = parse_expression(p)?;
    Some(WalrusStmt { ident, value })
}

fn parse_while(p: &mut Parser) -> Option<WhileStmt> {
    p.expect(&TokenKind::While, "'while'");
    let condition = parse_expression(p)?;
    let body = parse_codeblock(p)?;
    Some(WhileStmt { condition, body })
}

/// `for init; cond; post { ... }` -- three semicolon-separated clauses, each
/// independently optional, with no enclosing parens (unlike C). `init`
/// reuses the same shapes `Statement` already has for declare-and-assign
/// (`Walrus`, `Declaration`(`WithInit`)) or a plain expression; `return`/
/// `extern`/`struct`/`defer` aren't included: none of them make sense as a
/// loop's init clause. The `post` clause sits directly before the mandatory
/// body `{...}` with no separating `;`, and a bare `{...}` is itself a
/// valid expression -- so an *empty* post clause has to be told apart from
/// "the post clause is empty and this `{` is the body" by checking for `{`
/// first, with no attempt to parse an expression there at all (the old
/// grammar used a zero-width `.rewind()` for the same purpose; a plain peek
/// does the same job here with no backtracking needed).
///
/// If any clause fails to parse, recovery is local and specific to this
/// construct rather than delegating to the generic statement-level
/// synchronizer: `for`'s own two internal `;`s sit at bracket depth 0,
/// indistinguishable by the generic synchronizer from a real statement
/// terminator (see `parser::recovery`'s module doc comment) -- so instead,
/// this scans forward for its own body's opening `{` and skips the whole
/// body as one balanced unit, leaving the cursor positioned right after
/// this (entire, if malformed) `for` statement, ready for whatever comes
/// next, rather than resynchronizing mid-header.
fn parse_for(p: &mut Parser) -> Option<ForStmt> {
    p.expect(&TokenKind::For, "'for'");
    let init = parse_for_init(p);
    if !p.expect(&TokenKind::Semi, "';'") {
        return recover_for_header(p);
    }
    let condition = if p.check(&TokenKind::Semi) { None } else { parse_expression(p) };
    if !p.expect(&TokenKind::Semi, "';'") {
        return recover_for_header(p);
    }
    let post = if p.check(&TokenKind::LBrace) { None } else { parse_expression(p) };
    let Some(body) = parse_codeblock(p) else { return recover_for_header(p) };
    Some(ForStmt { init, condition, post, body })
}

fn parse_for_init(p: &mut Parser) -> Option<Statement> {
    if p.check(&TokenKind::Semi) {
        return None;
    }
    if matches!(p.peek(), TokenKind::Ident(_)) && matches!(p.peek_at(1), TokenKind::ColonEq) {
        return parse_walrus(p).map(Statement::Walrus);
    }
    if matches!(p.peek(), TokenKind::Ident(_)) && matches!(p.peek_at(1), TokenKind::Colon) {
        let decl = parse_declaration(p)?;
        return if p.eat(&TokenKind::Eq) {
            let value = parse_expression(p)?;
            Some(Statement::DeclarationWithInit(decl, value))
        } else {
            Some(Statement::Declaration(decl))
        };
    }
    parse_expression(p).map(Statement::Expression)
}

/// See `parse_for`'s doc comment. Scans forward (bracket-depth-aware, so a
/// `{`/`;` nested inside e.g. a call's argument list is never mistaken for
/// the loop's own boundary) for this `for`'s own body's `{`, then skips the
/// whole body as one balanced unit -- or, if a top-level-item-looking token
/// or EOF is hit first, stops there instead, leaving the rest to the
/// caller's own recovery.
fn recover_for_header(p: &mut Parser) -> Option<ForStmt> {
    let mut depth = 0usize;
    loop {
        match p.peek() {
            TokenKind::Eof => return None,
            TokenKind::LBrace if depth == 0 => {
                recovery::skip_balanced_group(p);
                return None;
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth += 1;
                p.advance();
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                p.advance();
            }
            TokenKind::Extern | TokenKind::Import | TokenKind::Macro if depth == 0 => return None,
            _ => {
                p.advance();
            }
        }
    }
}
