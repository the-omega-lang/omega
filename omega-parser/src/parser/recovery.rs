//! Panic-mode error recovery: after a construct fails to parse, skip
//! forward to a plausible boundary and keep going, so one mistake produces
//! one error instead of aborting the whole file. Two granularities --
//! top-level item boundaries and function-body statement boundaries -- not
//! finer-grained; a malformed sub-expression already means its enclosing
//! statement can't be trusted, so bailing to the nearest statement/item
//! boundary is the standard, sufficient granularity real compilers use.
//!
//! Both helpers here are bracket-depth-aware: an entire `(...)`/`[...]`/
//! `{...}` is skipped as one atomic unit regardless of what's inside it, so
//! e.g. a `;` inside a function call's argument list is never mistaken for
//! a statement terminator.
//!
//! **Not** handled here: `for init; cond; post { ... }`'s own two internal,
//! unparenthesized clause-separator `;`s. If an error occurs while parsing
//! one of `for`'s own clauses, the parser is already lexically *inside* the
//! header at that point -- from there, the very next depth-0 `;` this
//! module's generic scan would find genuinely *is* the for-loop's own next
//! clause separator, not an unrelated statement terminator, so stopping
//! there would resynchronize mid-header and cascade into spurious errors on
//! the rest of an otherwise-valid loop. That's a distinct, local problem
//! only `for`'s own parsing function is in a position to fix (by scanning
//! forward for its own body's `{` and skipping the whole body as a unit
//! instead) -- see `parser::statement`'s `parse_for`.

use crate::lexer::TokenKind;
use crate::parser::Parser;

/// Recovers after a failed top-level item: skips to the next depth-0 `;`
/// (consumed) or a token that plausibly starts a new item, whichever comes
/// first. A stray depth-0 `}` is consumed and skipped -- there is no
/// enclosing block at item level for it to close, and stopping *at* it
/// (like statement recovery does) would stall `parse_source_module` on the
/// same token forever.
pub fn synchronize_to_item_boundary(p: &mut Parser) {
    synchronize(p, starts_item, false);
}

/// Recovers after a failed statement: skips to the next depth-0 `;`
/// (consumed) or a token that plausibly starts a new statement, whichever
/// comes first -- additionally stopping (without consuming) at an
/// unconsumed `}`, so the enclosing block still finishes cleanly instead of
/// recovery eating its way out of the block entirely.
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

/// Consumes a `(...)`/`[...]`/`{...}` group wholesale, tracking nesting
/// depth across all three bracket kinds (not per-kind -- true of any
/// single-depth-counter scan, and acceptable here since a well-formed
/// source never crosses delimiters like `{ ( }`; recovery is already
/// operating on malformed input, so this only needs to be a reasonable
/// best effort, not a perfect one). Assumes `p.peek()` is currently one of
/// the three opening delimiters. `pub(crate)`: also used by
/// `parser::statement::recover_for_header` for its own local, `for`-header-
/// specific recovery (see that function's doc comment).
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
            | TokenKind::Macro
            | TokenKind::Ident(_)
    )
}

fn starts_statement(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::If
            | TokenKind::While
            | TokenKind::For
            | TokenKind::Struct
            | TokenKind::Union
            | TokenKind::Return
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Defer
            | TokenKind::Ident(_)
    )
}
