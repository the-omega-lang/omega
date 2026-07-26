use crate::span::Span;

/// A coarse lexical class, just fine-grained enough to colorize a snippet
/// line -- deliberately much coarser than the real `TokenKind` so this
/// crate never has to know the language's actual token set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenClass {
    Keyword,
    /// String *and* character literals -- conventionally colored alike.
    String,
    Number,
    Comment,
}

/// Classifies regions of a source file for snippet colorization. Implemented
/// by `omega_parser` (which owns the real lexer) and injected into
/// [`crate::Renderer`] by the CLI -- the dependency arrow stays
/// parser-depends-on-diagnostics, never the reverse.
///
/// Returned spans must be sorted by `start` and non-overlapping (any honest
/// lexer's token stream already is). Anything not covered renders in the
/// terminal's default color. Implementations must be error-tolerant: the
/// whole point is highlighting *broken* source, so a lex error just means
/// "no class for that region", never a failure.
pub trait Highlighter {
    fn highlight(&self, source: &str) -> Vec<(Span, TokenClass)>;
}
