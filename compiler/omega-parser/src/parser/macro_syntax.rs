use crate::ast::expression::macro_invocation::MacroInvocationExpr;
use crate::ast::identifier::Ident;
use crate::ast::statement::macro_definition::{
    FragmentKind, MacroBodyPiece, MacroDefinitionStmt, MacroParam, MacroRepetition, MacroSignature,
};
use crate::ast::visibility::Visibility;
use crate::diagnostics::ParseErrorKind;
use crate::lexer::{Token, TokenKind};
use crate::parser::Parser;

/// `name$(arg, ...)`, shared by item, statement, and expression position.
pub fn parse_macro_invocation(p: &mut Parser) -> Option<MacroInvocationExpr> {
    let name = p.expect_ident()?;
    p.expect(&TokenKind::Dollar, "'$'");
    p.expect(&TokenKind::LParen, "'('");
    let args = parse_macro_args(p)?;
    p.expect(&TokenKind::RParen, "')'");
    Some(MacroInvocationExpr { name, args })
}

/// Captures comma-separated raw argument token runs. This is deliberately the
/// only remaining use of `capture_token_run`.
fn parse_macro_args(p: &mut Parser) -> Option<Vec<Vec<Token>>> {
    let mut args = Vec::new();
    if p.check(&TokenKind::RParen) {
        return Some(args);
    }
    loop {
        let arg = capture_token_run(p, |k| matches!(k, TokenKind::Comma | TokenKind::RParen));
        if arg.is_empty() {
            p.error(ParseErrorKind::Expected {
                expected: "a macro argument",
                found: p.peek().describe(),
            });
            return None;
        }
        args.push(arg);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
        if p.check(&TokenKind::RParen) {
            p.error(ParseErrorKind::Expected {
                expected: "a macro argument",
                found: p.peek().describe(),
            });
            return None;
        }
    }
    Some(args)
}

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

/// `macro name($a: expr, $rest: expr...) => { ... }`.
pub fn parse_macro_definition(
    p: &mut Parser,
    visibility: Visibility,
) -> Option<MacroDefinitionStmt> {
    p.expect(&TokenKind::Macro, "'macro'");
    let name = p.expect_ident()?;
    p.expect(&TokenKind::LParen, "'('");
    let signature = parse_macro_signature(p)?;
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    p.expect(&TokenKind::LBrace, "'{'");
    let body = parse_macro_body(p, false)?;
    p.expect(&TokenKind::RBrace, "'}'");
    Some(MacroDefinitionStmt {
        visibility,
        name,
        signature,
        body,
    })
}

fn parse_macro_signature(p: &mut Parser) -> Option<MacroSignature> {
    let mut fixed = Vec::new();
    let mut variadic = None;
    if !matches!(p.peek(), TokenKind::Metavar(_)) {
        return Some(MacroSignature { fixed, variadic });
    }
    loop {
        let TokenKind::Metavar(name) = p.peek().clone() else {
            p.error(ParseErrorKind::Expected {
                expected: "a macro parameter ('$name')",
                found: p.peek().describe(),
            });
            return None;
        };
        p.advance();
        p.expect(&TokenKind::Colon, "':'");
        let kind = parse_fragment_kind(p)?;
        let param = MacroParam {
            name: Ident(name),
            kind,
        };
        if p.eat(&TokenKind::DotDotDot) {
            variadic = Some(param);
            if p.check(&TokenKind::Comma) {
                p.error(ParseErrorKind::VariadicMacroParamNotLast);
                return None;
            }
            break;
        }
        fixed.push(param);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
    }
    Some(MacroSignature { fixed, variadic })
}

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
        TokenKind::Ident(name) if name == "ident" => {
            p.advance();
            Some(FragmentKind::Ident)
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "'expr', 'type' or 'ident'",
                found: p.peek().describe(),
            });
            None
        }
    }
}

/// Parses a body tree. Ordinary bracketed groups stay flat token pieces
/// (only their depth is tracked, so the depth-0 `}` that closes this body is
/// recognized); a repetition is recognized by the fixed two-token prefix
/// `$` `...` at *any* depth -- `$f($...(,){ $args })` deliberately puts one
/// inside an argument list. `in_repetition` is what rejects a repetition
/// nested inside another one, at any depth.
fn parse_macro_body(p: &mut Parser, in_repetition: bool) -> Option<Vec<MacroBodyPiece>> {
    let mut body = Vec::new();
    let mut depth = 0usize;
    loop {
        match p.peek() {
            TokenKind::Eof => break,
            TokenKind::RBrace if depth == 0 => break,
            TokenKind::Dollar if matches!(p.peek_at(1), TokenKind::DotDotDot) => {
                if in_repetition {
                    p.error(ParseErrorKind::NestedMacroRepetition);
                    return None;
                }
                body.push(MacroBodyPiece::Repetition(parse_repetition(p)?));
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth += 1;
                body.push(MacroBodyPiece::Token(p.advance()));
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                body.push(MacroBodyPiece::Token(p.advance()));
            }
            _ => body.push(MacroBodyPiece::Token(p.advance())),
        }
    }
    Some(body)
}

fn parse_repetition(p: &mut Parser) -> Option<MacroRepetition> {
    let start = p.peek_span();
    p.expect(&TokenKind::Dollar, "'$'");
    p.expect(&TokenKind::DotDotDot, "'...'");
    p.expect(&TokenKind::LParen, "'('");
    let separator = if p.check(&TokenKind::RParen) {
        None
    } else if matches!(
        p.peek(),
        TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::LBrace
            | TokenKind::RBrace
    ) {
        p.error(ParseErrorKind::InvalidMacroSeparator);
        return None;
    } else {
        let sep = p.advance();
        if !p.check(&TokenKind::RParen) {
            p.error(ParseErrorKind::InvalidMacroSeparator);
            return None;
        }
        Some(sep)
    };
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::LBrace, "'{'");
    let body = parse_macro_body(p, true)?;
    p.expect(&TokenKind::RBrace, "'}'");
    Some(MacroRepetition {
        separator,
        body,
        span: start.to(p.last_span()),
    })
}
