//! Retired pointer and mutability spellings. `mut` is an ordinary name, so
//! each check below matches only a juxtaposition that can never be valid
//! Omega, and reports it instead of letting it cascade into unrelated errors.

use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, contextual};

const MUT: &str = "mut";

pub fn mut_at(p: &Parser, offset: usize) -> bool {
    p.at_contextual_at(offset, MUT)
}

/// `mut self` or `*mut self`, at `offset`.
pub fn mut_self_at(p: &Parser, offset: usize) -> bool {
    mut_at(p, offset) && p.at_contextual_at(offset + 1, contextual::SELF)
}

/// `mut` written where `^` belongs, directly before a declared name, which
/// `name_follows` recognises at the given offset. Consumes and reports it.
pub fn eat_mut_before(p: &mut Parser, name_follows: impl Fn(&Parser, usize) -> bool) -> bool {
    if !(mut_at(p, 0) && name_follows(p, 1)) {
        return false;
    }
    p.error(ParseErrorKind::LegacyMutBinding);
    p.advance();
    true
}

/// `*mut T`: `mut` directly followed by something that starts a type, which a
/// pointer to a type named `mut` never is.
pub fn mut_pointer_at(p: &Parser, offset: usize) -> bool {
    mut_at(p, offset)
        && matches!(
            p.peek_at(offset + 1),
            TokenKind::Ident(_)
                | TokenKind::Star
                | TokenKind::Caret
                | TokenKind::LBracket
                | TokenKind::LParen
                | TokenKind::Spec
                | TokenKind::Enum
                | TokenKind::Foreign
        )
}
