
use crate::lexer::TokenKind;
use crate::parser::Parser;

pub fn synchronize_to_item_boundary(p: &mut Parser) {
    synchronize(p, starts_item, false);
}

pub fn synchronize_to_statement_boundary(p: &mut Parser) {
    synchronize(p, starts_statement, true);
}

fn synchronize(p: &mut Parser, starts_boundary: fn(&TokenKind) -> bool, stop_at_rbrace: bool) {
    loop {
        match p.peek() {
            TokenKind::Eof => return,
            TokenKind::RBrace if stop_at_rbrace => return,
            TokenKind::Semi => {
                p.advance();
                return;
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                skip_balanced_group(p);
            }
            kind if starts_boundary(kind) => return,
            _ => {
                p.advance();
            }
        }
    }
}

pub(crate) fn skip_balanced_group(p: &mut Parser) {
    let mut depth = 0usize;
    loop {
        match p.peek() {
            TokenKind::Eof => return,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth += 1;
                p.advance();
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                p.advance();
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {
                p.advance();
            }
        }
    }
}

fn starts_item(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Extern
            | TokenKind::Import
            | TokenKind::Struct
            | TokenKind::Union
            | TokenKind::Spec
            | TokenKind::Macro
            | TokenKind::Ident(_)
    )
}

fn starts_statement(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::If
            | TokenKind::While
            | TokenKind::Loop
            | TokenKind::For
            | TokenKind::Struct
            | TokenKind::Union
            | TokenKind::Spec
            | TokenKind::Return
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Defer
            | TokenKind::Ident(_)
    )
}
