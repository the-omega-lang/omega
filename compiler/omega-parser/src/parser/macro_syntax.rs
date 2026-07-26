use crate::ast::expression::macro_invocation::MacroInvocationExpr;
use crate::ast::identifier::Ident;
use crate::ast::statement::macro_definition::{FragmentKind, MacroDefinitionStmt, MacroOutputKind, MacroParam};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::{Token, TokenKind};
use crate::parser::Parser;

/// `name!(arg, ...)` -- shared verbatim between expression position and
/// module-top-level item position; see `MacroInvocationExpr`'s own doc
/// comment. Callers already peek the leading `Ident` + `!` to decide to
/// call this at all (see `parser::expression::parse_primary`/
/// `parser::item::parse_item`).
pub fn parse_macro_invocation(p: &mut Parser) -> Option<MacroInvocationExpr> {
    let name = p.expect_ident()?;
    p.expect(&TokenKind::Bang, "'!'");
    p.expect(&TokenKind::LParen, "'('");
    let args = parse_macro_args(p)?;
    p.expect(&TokenKind::RParen, "')'");
    Some(MacroInvocationExpr { name, args })
}

/// Captures each comma-separated argument as a raw token slice (not parsed
/// as an `Expression`/`Type` here -- a `Type`-fragment argument, e.g.
/// `generate_type!(Counter)`, isn't valid expression syntax; see
/// `omega_parser::macros` for where each argument is validated against its
/// parameter's declared `FragmentKind` and substituted). Since the whole
/// source is already tokenized up front, this is a simple bracket-depth
/// scan over already-lexed tokens -- no separate re-tokenization pass is
/// needed the way the old macro-only `Scanner`/`balanced_content` needed
/// (a `)`/`,` inside a string literal was already consumed as part of that
/// `Str` token during lexing, so it can never be mistaken for an argument
/// boundary here).
fn parse_macro_args(p: &mut Parser) -> Option<Vec<Vec<Token>>> {
    let mut args = Vec::new();
    if p.check(&TokenKind::RParen) {
        return Some(args);
    }
    loop {
        let arg = capture_token_run(p, |k| matches!(k, TokenKind::Comma | TokenKind::RParen));
        if arg.is_empty() {
            p.error(ParseErrorKind::Expected { expected: "a macro argument", found: p.peek().describe() });
            return None;
        }
        args.push(arg);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
        if p.check(&TokenKind::RParen) {
            p.error(ParseErrorKind::Expected { expected: "a macro argument", found: p.peek().describe() });
            return None;
        }
    }
    Some(args)
}

/// Captures raw tokens up to (not including) the first depth-0 token
/// matching `stop` -- bracket-depth-aware, so a `,`/`)`/`}` nested inside a
/// `(...)`/`[...]`/`{...}` never ends the run early. Used both for one
/// invocation argument (stopping at `,`/`)`) and for a whole macro
/// definition's body (stopping at the matching `}`).
fn capture_token_run(p: &mut Parser, stop: impl Fn(&TokenKind) -> bool) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut depth = 0usize;
    loop {
        match p.peek() {
            TokenKind::Eof => break,
            kind if depth == 0 && stop(kind) => break,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth += 1;
                tokens.push(p.advance());
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                tokens.push(p.advance());
            }
            _ => tokens.push(p.advance()),
        }
    }
    tokens
}

/// `macro name($a: expr, $b: type, ...) => expr|items { ... }` -- the body
/// is captured as a raw token slice via the same bracket-depth mechanism as
/// invocation arguments, never parsed as real grammar here (it legitimately
/// contains `$name` metavariables and, for an `Items`-output macro, syntax
/// only valid post-substitution, e.g. `struct $name { ... }`).
pub fn parse_macro_definition(p: &mut Parser) -> Option<MacroDefinitionStmt> {
    p.expect(&TokenKind::Macro, "'macro'");
    let name = p.expect_ident()?;
    p.expect(&TokenKind::LParen, "'('");
    let params = parse_macro_params(p)?;
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let output = parse_macro_output_kind(p)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let body = capture_token_run(p, |k| matches!(k, TokenKind::RBrace));
    p.expect(&TokenKind::RBrace, "'}'");
    Some(MacroDefinitionStmt { name, params, output, body })
}

/// `$name: expr`/`$name: type`, comma-separated -- `$name` is always
/// lexed as one atomic `Metavar` token (see `lexer::TokenKind`'s doc
/// comment), so there's no separate `$` punctuation to pair up here.
fn parse_macro_params(p: &mut Parser) -> Option<Vec<MacroParam>> {
    let mut params = Vec::new();
    if !matches!(p.peek(), TokenKind::Metavar(_)) {
        return Some(params);
    }
    loop {
        let TokenKind::Metavar(name) = p.peek().clone() else {
            p.error(ParseErrorKind::Expected { expected: "a macro parameter ('$name')", found: p.peek().describe() });
            return None;
        };
        p.advance();
        p.expect(&TokenKind::Colon, "':'");
        let kind = parse_fragment_kind(p)?;
        params.push(MacroParam { name: Ident(name), kind });
        if matches!(p.peek(), TokenKind::Comma) && matches!(p.peek_at(1), TokenKind::Metavar(_)) {
            p.advance();
        } else {
            break;
        }
    }
    Some(params)
}

/// `expr`/`type` -- contextual keywords (see `lexer::TokenKind`'s doc
/// comment on why these aren't global lexer keywords), recognized by
/// comparing an already-lexed `Ident`'s text, exactly where the grammar
/// actually needs them to mean something special.
fn parse_fragment_kind(p: &mut Parser) -> Option<FragmentKind> {
    match p.peek() {
        TokenKind::Ident(name) if name == "expr" => {
            p.advance();
            Some(FragmentKind::Expr)
        }
        TokenKind::Ident(name) if name == "type" => {
            p.advance();
            Some(FragmentKind::Type)
        }
        _ => {
            p.error(ParseErrorKind::Expected { expected: "'expr' or 'type'", found: p.peek().describe() });
            None
        }
    }
}

/// `expr`/`items` -- see `parse_fragment_kind`'s doc comment; same
/// contextual-keyword treatment.
fn parse_macro_output_kind(p: &mut Parser) -> Option<MacroOutputKind> {
    match p.peek() {
        TokenKind::Ident(name) if name == "expr" => {
            p.advance();
            Some(MacroOutputKind::Expr)
        }
        TokenKind::Ident(name) if name == "items" => {
            p.advance();
            Some(MacroOutputKind::Items)
        }
        _ => {
            p.error(ParseErrorKind::Expected { expected: "'expr' or 'items'", found: p.peek().describe() });
            None
        }
    }
}
