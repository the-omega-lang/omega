pub mod expression;
pub mod item;
pub mod macro_syntax;
pub mod recovery;
pub mod statement;
pub mod r#type;

use crate::diagnostics::{ParseError, ParseErrorKind, Span};
use crate::lexer::{Token, TokenKind};

/// A recursive-descent parser over an already-lexed token stream (see
/// `crate::lexer::tokenize`) -- `omega-parser` no longer parses directly
/// against `&str` the way the old chumsky grammar did, which is what lets
/// every parsing function stay unaware of trivia (already stripped out
/// during lexing) and reason about lookahead in terms of whole tokens
/// rather than characters.
///
/// `mark`/`reset` is the backtracking primitive, used sparingly -- most of
/// this grammar's disambiguation only needs a bounded, non-consuming peek
/// (e.g. "is the token after this identifier a `:` or a `:=`?"), not real
/// backtracking; root-item dispatch, for instance, needs none at all (a
/// single-token lookahead already tells `:`/`(`/`!` apart, see
/// `parser::item::parse_declaration_or_function_definition`). The one
/// genuine backtracking site is `parser::expression::parse_codeblock`'s
/// tail-vs-statement disambiguation, which has no cheaper way to tell "this
/// expression is the block's tail" from "this is the start of an ordinary
/// statement" apart than trying the expression interpretation and checking
/// what follows. `reset` also truncates `errors` back to the mark's count --
/// so a speculative attempt that gets abandoned via `reset` never leaves
/// behind spurious errors from the road not taken.
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    errors: Vec<ParseError>,
    /// Rust's "no struct literal here" restriction: inside an `if`/`while`
    /// condition or a `for` header, `flag { ... }` must mean "condition
    /// `flag`, then the body block", never a struct literal -- the two are
    /// otherwise syntactically identical. Set by those condition parsers
    /// (see `restrict_struct_literals`), and cleared again the moment the
    /// grammar enters any bracketed sub-context (`(...)`, `[...]`, `{...}`),
    /// where a `{` can no longer be mistaken for the statement's own body --
    /// the exact rule rustc's parser applies.
    struct_literals_restricted: bool,
}

/// A saved parser position, from `Parser::mark` -- opaque; only meaningful
/// as an argument back to `Parser::reset` on the same `Parser`.
#[derive(Clone, Copy)]
pub struct Mark {
    pos: usize,
    error_count: usize,
}

impl<'a> Parser<'a> {
    /// `tokens` must end with a `TokenKind::Eof` sentinel (as
    /// `lexer::tokenize` always produces) -- `peek`/`advance` rely on being
    /// able to sit at that final index forever without going out of bounds.
    pub fn new(tokens: &'a [Token]) -> Self {
        debug_assert!(matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Eof)));
        Self { tokens, pos: 0, errors: Vec::new(), struct_literals_restricted: false }
    }

    /// Whether a struct literal may start at the current position -- see
    /// `struct_literals_restricted`.
    pub fn struct_literals_allowed(&self) -> bool {
        !self.struct_literals_restricted
    }

    /// Runs `f` with struct literals restricted -- for `if`/`while`/`for`
    /// condition position, where `name { ... }` must parse as "condition,
    /// then body". The previous state is restored afterward, whatever it was.
    pub fn restrict_struct_literals<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = std::mem::replace(&mut self.struct_literals_restricted, true);
        let result = f(self);
        self.struct_literals_restricted = previous;
        result
    }

    /// Runs `f` with struct literals allowed again -- for every bracketed
    /// sub-context (`(...)`, `[...]`, `{...}`) inside a restricted position,
    /// where a `{` is unambiguous again. The previous state is restored
    /// afterward, whatever it was.
    pub fn allow_struct_literals<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = std::mem::replace(&mut self.struct_literals_restricted, false);
        let result = f(self);
        self.struct_literals_restricted = previous;
        result
    }

    pub fn into_errors(self) -> Vec<ParseError> {
        self.errors
    }

    pub fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    pub fn peek_at(&self, n: usize) -> &TokenKind {
        let idx = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    pub fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    /// The span of the most recently *consumed* token (i.e. `advance`'s
    /// last return value) -- the usual way a parsing function computes its
    /// own overall span once it's finished: `start.to(p.last_span())`.
    pub fn last_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    /// Consumes and returns the current token, unless it's the trailing
    /// `Eof` sentinel -- which is never actually consumed, so a parsing
    /// function that keeps calling `advance()` past the end of input just
    /// keeps observing `Eof` rather than panicking on an out-of-bounds
    /// index.
    pub fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        tok
    }

    pub fn mark(&self) -> Mark {
        Mark { pos: self.pos, error_count: self.errors.len() }
    }

    pub fn reset(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.errors.truncate(mark.error_count);
    }

    /// Does the current token equal `kind` exactly? For payload-bearing
    /// variants (`Ident`/`Number`/...) this only matches a specific payload
    /// value -- most call sites want `matches!(p.peek(), TokenKind::Ident(_))`
    /// instead when any payload is acceptable.
    pub fn check(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    /// Consumes the current token if it equals `kind`, with no error if it
    /// doesn't -- for genuinely optional tokens (e.g. a tolerated trailing
    /// `;` after a block-shaped statement).
    pub fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consumes the current token if it equals `kind`; otherwise records an
    /// `Expected` error (without consuming anything) and returns `false`.
    pub fn expect(&mut self, kind: &TokenKind, expected: &'static str) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error(ParseErrorKind::Expected { expected, found: self.peek().describe() });
            false
        }
    }

    /// Like `expect`, but anchors the error *just after the previously
    /// consumed token* (zero-width) instead of at whatever token comes
    /// next -- for a statement terminator like `;`, "add it at the end of
    /// what you just wrote" is where the fix belongs, which may be a whole
    /// line away from wherever the next token happens to start.
    pub fn expect_terminator(&mut self, kind: &TokenKind, expected: &'static str) -> bool {
        if self.eat(kind) {
            return true;
        }
        let after_last = self.last_span().end;
        self.error_at(
            Span::new(after_last, after_last),
            ParseErrorKind::Expected { expected, found: self.peek().describe() },
        );
        false
    }

    /// Consumes the current token if it's an `Ident`, returning its name;
    /// otherwise records an `Expected` error and returns `None`.
    pub fn expect_ident(&mut self) -> Option<crate::ast::identifier::Ident> {
        if let TokenKind::Ident(name) = self.peek() {
            let name = name.clone();
            self.advance();
            Some(crate::ast::identifier::Ident(name))
        } else {
            self.error(ParseErrorKind::Expected { expected: "an identifier", found: self.peek().describe() });
            None
        }
    }

    pub fn error(&mut self, kind: ParseErrorKind) {
        self.error_at(self.peek_span(), kind);
    }

    pub fn error_at(&mut self, span: Span, kind: ParseErrorKind) {
        self.errors.push(ParseError::new(span, kind));
    }
}

/// `a`, or `a::b::c` -- shared by type position, expression position, and
/// `import` statements alike (see `ast::identifier::Path`'s own doc
/// comment). `::` is matched as one atomic token by the lexer already
/// (maximal munch), so there's no risk of it being mistaken for two bare
/// `:`s here.
pub fn parse_path(p: &mut Parser) -> Option<crate::ast::identifier::Path> {
    let head = p.expect_ident()?;
    let mut tail = Vec::new();
    while p.check(&TokenKind::ColonColon) {
        p.advance();
        match p.expect_ident() {
            Some(seg) => tail.push(seg),
            None => break,
        }
    }
    Some(crate::ast::identifier::Path { head, tail })
}
