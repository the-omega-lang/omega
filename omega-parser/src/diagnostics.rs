use std::fmt;

/// A byte range into one source file -- deliberately *not* tagged with
/// which file, matching how this has always worked (the old
/// `chumsky::span::SimpleSpan` this replaces had a `context: C = ()` slot
/// that was never actually used): `Driver` already threads a module's file
/// path alongside every span it touches, so embedding file identity here
/// would ripple through every `Span` field in `omega-hir`/`omega-analyzer`
/// for no benefit -- nothing there ever compares spans *across* files.
///
/// Composite spans (covering more than one token, e.g. a whole
/// `BinaryOpExpr`) are built as `(min of every constituent token's start,
/// max of every constituent token's end)`, not "first token's start, last
/// token's end" -- see `omega_parser::macros`, where a node built from
/// tokens spliced in from two different source locations (a macro's
/// definition site and its invocation site) could otherwise produce a
/// non-contiguous or even inverted (`start > end`) span. `min`/`max`
/// construction is always well-formed regardless of where the constituent
/// tokens originated, even though it may not describe a single contiguous
/// range in that case -- callers must not assume a `Span` is always one
/// contiguous highlighted region, only that `start <= end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "Span::new: start ({start}) > end ({end})");
        Self { start, end }
    }

    /// The smallest span covering both `self` and `other` -- the `min`
    /// start/`max` end construction described above.
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}

/// A source file's newline offsets, precomputed once, so translating a byte
/// offset into a 1-based `(line, column)` pair is a binary search rather
/// than a re-scan of the whole file for every diagnostic. Columns count
/// Unicode scalar values (`char`s), not bytes or grapheme clusters -- good
/// enough for a terminal-caret-style renderer without pulling in a full
/// grapheme-segmentation dependency.
pub struct LineIndex {
    /// Byte offset of the start of each line; `line_starts[0]` is always 0.
    line_starts: Vec<usize>,
    source: String,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, c) in source.char_indices() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts, source: source.to_string() }
    }

    /// 1-based `(line, column)` for a byte offset. Offsets past the end of
    /// the source clamp to the last line/column rather than panicking --
    /// diagnostics can legitimately point just past EOF (e.g. "expected `}`
    /// but found end of input").
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.source.len());
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(insert_at) => insert_at - 1,
        };
        let line_start = self.line_starts[line_idx];
        let column = self.source[line_start..offset].chars().count() + 1;
        (line_idx + 1, column)
    }

    /// The 1-based `line`'s own text, without its trailing newline -- for a
    /// one-line diagnostic snippet. Empty string for an out-of-range line.
    pub fn line_text(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line.wrapping_sub(1)) else { return "" };
        let end = self
            .line_starts
            .get(line)
            .map(|&next| next.saturating_sub(1))
            .unwrap_or(self.source.len());
        self.source[start..end.max(start)].trim_end_matches('\r')
    }
}

/// One parse-time problem, anchored at the span it concerns. Recoverable:
/// `omega_parser`'s lexer/parser keep going after producing one of these
/// (see `parser::recovery`), collecting as many as it can into one
/// `Vec<ParseError>` rather than stopping at the first.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub kind: ParseErrorKind,
}

impl ParseError {
    pub fn new(span: Span, kind: ParseErrorKind) -> Self {
        Self { span, kind }
    }
}

/// A short, human-readable name for what was actually found at a failure
/// point -- built directly from a `TokenKind` by the lexer/parser, kept as
/// an owned `String` here (rather than borrowing a `Token`) so a
/// `ParseError` never needs to outlive the token stream it was produced
/// from.
pub type TokenDescription = String;

#[derive(Debug, Clone)]
pub enum ParseErrorKind {
    /// The general-purpose "this grammar rule didn't match" case, covering
    /// most parser call sites -- `expected` is a short, static description
    /// of what the parser was looking for (e.g. `"a type"`, `"';'"`,
    /// `"an expression"`).
    Expected { expected: &'static str, found: TokenDescription },
    UnterminatedString,
    UnterminatedChar,
    UnterminatedComment,
    /// A macro-body/argument capture (`{ ... }`/`( ... )`) never found its
    /// matching close delimiter before EOF.
    UnterminatedGroup { open: char },
    InvalidCharacter(char),
    /// A `$` not immediately followed by an identifier -- `$` has exactly
    /// one meaning in this grammar (a metavariable reference).
    InvalidMetavariable,
    InvalidUnicodeEscape(String),
    /// An empty character literal (`''`), or one containing more than one
    /// character/escape.
    InvalidCharLiteral,
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected { expected, found } => write!(f, "expected {expected}, found {found}"),
            Self::UnterminatedString => write!(f, "unterminated string literal"),
            Self::UnterminatedChar => write!(f, "unterminated character literal"),
            Self::UnterminatedComment => write!(f, "unterminated comment"),
            Self::UnterminatedGroup { open } => write!(f, "unterminated '{open}' (no matching close found)"),
            Self::InvalidCharacter(c) => write!(f, "unexpected character '{c}'"),
            Self::InvalidMetavariable => write!(f, "expected an identifier after '$'"),
            Self::InvalidUnicodeEscape(hex) => write!(f, "invalid unicode escape '\\u{{{hex}}}'"),
            Self::InvalidCharLiteral => write!(f, "character literal must contain exactly one character"),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

/// Renders every error in `errors` as `<path>:<line>:<col>: <message>`
/// followed by the offending source line and a `^` caret under the column,
/// joined with blank lines between errors -- the "basic line:col+snippet"
/// rendering this rewrite's plan calls for, short of the fuller diagnostic
/// renderer (color, multi-span underlines, "help:" notes) left as future
/// work. `path` is display-only (e.g. `"mymodule::thing"` or a file path) --
/// callers already have this on hand (see `Driver::parse_module`), so it's
/// taken as a plain string rather than this crate inventing its own file-
/// identity concept.
pub fn render_errors(path: &str, source: &str, errors: &[ParseError]) -> String {
    let index = LineIndex::new(source);
    errors
        .iter()
        .map(|error| {
            let (line, col) = index.line_col(error.span.start);
            let snippet = index.line_text(line);
            let caret = " ".repeat(col.saturating_sub(1)) + "^";
            format!("{path}:{line}:{col}: {error}\n{snippet}\n{caret}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
