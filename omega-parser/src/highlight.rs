//! Omega's [`Highlighter`] implementation -- classifies a source file for
//! diagnostic-snippet colorization by running the real lexer over it, so
//! the colors in an error snippet can never disagree with what the
//! compiler actually lexed. Lex errors are simply dropped: broken regions
//! render unclassified (default color), which is exactly right for source
//! that is, after all, being quoted *because* it's broken.

use crate::lexer::{self, TokenKind};
use omega_diagnostics::{Highlighter, Span, TokenClass};

pub struct OmegaHighlighter;

impl Highlighter for OmegaHighlighter {
    fn highlight(&self, source: &str) -> Vec<(Span, TokenClass)> {
        let lexed = lexer::lex(source);
        let mut spans: Vec<(Span, TokenClass)> = Vec::new();
        for token in &lexed.tokens {
            let class = match &token.kind {
                TokenKind::True
                | TokenKind::False
                | TokenKind::If
                | TokenKind::Else
                | TokenKind::Extern
                | TokenKind::Import
                | TokenKind::Return
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Union
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Defer
                | TokenKind::Macro
                // A metavariable is macro syntax, not an ordinary name --
                // keyword coloring reads right.
                | TokenKind::Metavar(_) => TokenClass::Keyword,
                TokenKind::Str(_) | TokenKind::ByteStr(_) | TokenKind::Char(_) => TokenClass::String,
                TokenKind::Number(_) => TokenClass::Number,
                _ => continue,
            };
            spans.push((token.span, class));
        }
        for &span in &lexed.comments {
            spans.push((span, TokenClass::Comment));
        }
        // Tokens and comments each come out in source order but interleave;
        // the renderer requires one sorted, non-overlapping list.
        spans.sort_by_key(|(span, _)| span.start);
        spans
    }
}
