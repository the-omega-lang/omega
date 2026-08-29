use crate::lexer::TokenKind;
use crate::parser::Parser;

/// The construct a recovery step is trying to resynchronize to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Item,
    Statement,
}

pub fn synchronize_to_item_boundary(p: &mut Parser) {
    recover(p, Boundary::Item);
}

pub fn synchronize_to_statement_boundary(p: &mut Parser) {
    recover(p, Boundary::Statement);
}

/// Skips to the next credible construct, always consuming at least one token
/// unless the parser already sits on a boundary its caller owns. Without that
/// guarantee a construct that fails without consuming anything would make the
/// enclosing loop spin on the same token.
fn recover(p: &mut Parser, boundary: Boundary) {
    let before = p.position();
    synchronize(p, boundary);
    if p.position() == before && !at_enclosing_boundary(p) {
        p.advance();
    }
}

fn at_enclosing_boundary(p: &Parser) -> bool {
    matches!(p.peek(), TokenKind::RBrace | TokenKind::Eof)
}

fn synchronize(p: &mut Parser, boundary: Boundary) {
    loop {
        match p.peek() {
            TokenKind::Eof => return,
            // A closing brace belongs to the enclosing block, not to the
            // malformed construct: consuming it would make one bad member
            // swallow everything after its parent.
            TokenKind::RBrace => return,
            TokenKind::Semi => {
                p.advance();
                return;
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                skip_balanced_group(p);
            }
            _ if starts_boundary(p, boundary) => return,
            _ => {
                p.advance();
            }
        }
    }
}

fn starts_boundary(p: &Parser, boundary: Boundary) -> bool {
    match boundary {
        Boundary::Item => crate::parser::item::starts_item(p),
        Boundary::Statement => crate::parser::statement::starts_statement(p),
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
