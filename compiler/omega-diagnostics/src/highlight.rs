use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenClass {
    Keyword,
    String,
    Number,
    Comment,
}

pub trait Highlighter {
    fn highlight(&self, source: &str) -> Vec<(Span, TokenClass)>;
}
