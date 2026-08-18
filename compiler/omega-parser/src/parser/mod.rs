pub mod contextual;
pub mod expression;
pub mod item;
pub mod macro_syntax;
pub mod recovery;
pub mod statement;
pub mod r#type;

use crate::diagnostics::{ParseError, ParseErrorKind, Span};
use crate::lexer::{Token, TokenKind};

/// A recursive-descent parser over an already-lexed token stream (see
/// `crate::lexer::tokenize`), reasoning about lookahead in terms of whole
/// tokens rather than characters.
///
/// `mark`/`reset` is the backtracking primitive, used sparingly -- most
/// disambiguation only needs a bounded, non-consuming peek. The one genuine
/// backtracking site is `parser::expression::parse_codeblock`'s
/// tail-vs-statement disambiguation, which has no cheaper way to tell "this
/// expression is the block's tail" from "this is the start of an ordinary
/// statement" apart than trying the expression interpretation and checking
/// what follows. `reset` also truncates `errors` back to the mark's count,
/// so an abandoned speculative attempt never leaves spurious errors behind.
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    errors: Vec<ParseError>,
    /// Rust's "no struct literal here" restriction: inside an `if`/`while`
    /// condition or a `for` header, `flag { ... }` must mean "condition
    /// `flag`, then the body block", never a struct literal. Set by those
    /// condition parsers (see `restrict_struct_literals`), and cleared again
    /// once the grammar enters any bracketed sub-context (`(...)`, `[...]`,
    /// `{...}`), where a `{` can no longer be mistaken for the body.
    struct_literals_restricted: bool,
    /// The second half of a `>>` token, once `eat_close_angle`/
    /// `expect_close_angle` has split one in two (e.g. closing the outer
    /// generic of `Foo<Bar<Baz>>`) -- the lexer always lexes `>>` as one
    /// token (see `TokenKind::Shr`), so splitting it needs somewhere to
    /// stash the leftover `>` since `tokens` is an immutable borrowed slice.
    /// `peek`/`peek_at`/`advance` all consult this ahead of `tokens[pos]`
    /// whenever it's set, so every other parsing function keeps working
    /// unmodified once a split has happened.
    pending_gt: Option<Span>,
    /// The span of the most recently consumed token, tracked explicitly
    /// rather than derived from `tokens[pos - 1]` -- once `pending_gt`
    /// exists, the "previous token" may be a synthetic half of a split
    /// `>>`, which has no slot of its own in `tokens`.
    last_span: Span,
    /// How many grammar levels deep the recursive-descent parser currently
    /// is -- see `descend`. Deliberately *not* part of `Mark`: depth
    /// unwinds on its own as native frames return, so restoring it from a
    /// mark would corrupt it.
    depth: usize,
    /// Whether `MAX_NESTING_DEPTH` has already been reported once. Without
    /// this, `parse_block_contents`'s error recovery re-enters the grammar
    /// after each refusal and reports the same overflow again per statement.
    depth_exceeded: bool,
}

/// How many grammar levels (`parse_expression`/`parse_type` re-entries) may
/// nest before the parser refuses to descend further.
///
/// The parser is hand-written recursive descent, so grammar nesting costs
/// native stack -- roughly ten frames per level. A few hundred levels is
/// enough to exhaust a default 8MiB thread stack, and the failure mode
/// without this limit is a bare `fatal runtime error: stack overflow` with
/// no file, no line and no span.
///
/// 256 matches Clang's own `-fbracket-depth` default, far past anything
/// hand-written and reached only by generated source. This also bounds the
/// depth of the AST every later pass walks, so HIR lowering, analysis and
/// MIR inherit the bound for free.
pub const MAX_NESTING_DEPTH: usize = 256;

/// The lexer only ever collapses `>` this way as half of a wider token, so
/// the synthetic `Token` `eat_close_angle` hands back after a split always
/// carries this exact, dataless kind -- a `'static` constant lets `peek`
/// return `&TokenKind` for it with no owning storage in `Parser` itself.
const CLOSE_ANGLE_KIND: TokenKind = TokenKind::Gt;

/// A saved parser position, from `Parser::mark` -- opaque; only meaningful
/// as an argument back to `Parser::reset` on the same `Parser`.
#[derive(Clone, Copy)]
pub struct Mark {
    pos: usize,
    error_count: usize,
    pending_gt: Option<Span>,
    last_span: Span,
}

impl<'a> Parser<'a> {
    /// `tokens` must end with a `TokenKind::Eof` sentinel (as
    /// `lexer::tokenize` always produces) -- `peek`/`advance` rely on being
    /// able to sit at that final index forever without going out of bounds.
    pub fn new(tokens: &'a [Token]) -> Self {
        debug_assert!(matches!(
            tokens.last().map(|t| &t.kind),
            Some(TokenKind::Eof)
        ));
        Self {
            tokens,
            pos: 0,
            errors: Vec::new(),
            struct_literals_restricted: false,
            pending_gt: None,
            last_span: Span::new(0, 0),
            depth: 0,
            depth_exceeded: false,
        }
    }

    /// Runs `f` one grammar level deeper, refusing to recurse past
    /// `MAX_NESTING_DEPTH` and reporting `NestingTooDeep` instead.
    ///
    /// Save/run/restore, the same shape as `restrict_struct_literals` below:
    /// the recursive entry points are full of `?` early returns, so a
    /// `self.depth -= 1` at the end of a caller would be skipped on every
    /// error path and leak the count.
    ///
    /// Wrapped around the grammar's two genuine cycles -- `parse_expression`
    /// and `parse_type` -- with one shared counter rather than one per
    /// cycle, since what's being bounded is the native stack and a type
    /// nested inside an expression nested inside a type all draw on it.
    pub fn descend<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        if self.depth >= MAX_NESTING_DEPTH {
            if !std::mem::replace(&mut self.depth_exceeded, true) {
                self.error(ParseErrorKind::NestingTooDeep {
                    limit: MAX_NESTING_DEPTH,
                });
            }
            return None;
        }
        self.depth += 1;
        let result = f(self);
        self.depth -= 1;
        result
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
        if self.pending_gt.is_some() {
            return &CLOSE_ANGLE_KIND;
        }
        &self.tokens[self.pos].kind
    }

    pub fn peek_at(&self, n: usize) -> &TokenKind {
        if self.pending_gt.is_some() {
            if n == 0 {
                return &CLOSE_ANGLE_KIND;
            }
            let idx = (self.pos + n - 1).min(self.tokens.len() - 1);
            return &self.tokens[idx].kind;
        }
        let idx = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    pub fn peek_span(&self) -> Span {
        if let Some(span) = self.pending_gt {
            return span;
        }
        self.tokens[self.pos].span
    }

    /// The span of the most recently *consumed* token (i.e. `advance`'s
    /// last return value) -- the usual way a parsing function computes its
    /// own overall span once it's finished: `start.to(p.last_span())`.
    pub fn last_span(&self) -> Span {
        self.last_span
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
        if let Some(span) = self.pending_gt.take() {
            self.last_span = span;
            return Token {
                kind: TokenKind::Gt,
                span,
                origin: crate::ast::identifier::Origin::default(),
            };
        }
        let tok = self.tokens[self.pos].clone();
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
            self.last_span = tok.span;
        }
        tok
    }

    pub fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            error_count: self.errors.len(),
            pending_gt: self.pending_gt,
            last_span: self.last_span,
        }
    }

    pub fn reset(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.errors.truncate(mark.error_count);
        self.pending_gt = mark.pending_gt;
        self.last_span = mark.last_span;
    }

    /// Does the current token equal `kind` exactly? For payload-bearing
    /// variants (`Ident`/`Number`/...) this only matches a specific payload
    /// value -- most call sites want `matches!(p.peek(), TokenKind::Ident(_))`
    /// instead when any payload is acceptable.
    pub fn check(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    pub fn at_contextual(&self, keyword: &str) -> bool {
        self.at_contextual_at(0, keyword)
    }

    pub fn at_contextual_at(&self, offset: usize, keyword: &str) -> bool {
        matches!(self.peek_at(offset), TokenKind::Ident(name) if name == keyword)
    }

    pub fn eat_contextual(&mut self, keyword: &str) -> bool {
        if self.at_contextual(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub fn expect_contextual(&mut self, keyword: &'static str) -> bool {
        if self.eat_contextual(keyword) {
            true
        } else {
            self.error(ParseErrorKind::Expected {
                expected: keyword,
                found: self.peek().describe(),
            });
            false
        }
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
            self.error(ParseErrorKind::Expected {
                expected,
                found: self.peek().describe(),
            });
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
            ParseErrorKind::Expected {
                expected,
                found: self.peek().describe(),
            },
        );
        false
    }

    /// Consumes one `>` closing an angle-bracket construct (a generic
    /// argument/parameter list, a cast's own bracket, `sizeof<T>`) -- unlike
    /// a bare `eat(&TokenKind::Gt)`, this also splits a `>>` token into two
    /// logical `>`s when that's what's actually here, so a nested generic's
    /// own closing bracket (`Foo<Bar<Baz>>`) still closes correctly even
    /// though the lexer always lexes `>>` as one `Shr` token. Every
    /// closing-`>` site in the grammar must go through this (or
    /// `expect_close_angle`) rather than a bare `Gt` check.
    pub fn eat_close_angle(&mut self) -> bool {
        if self.eat(&TokenKind::Gt) {
            return true;
        }
        if matches!(self.peek(), TokenKind::Shr) {
            let whole = self.peek_span();
            let mid = whole.start + 1;
            self.pending_gt = Some(Span::new(mid, whole.end));
            self.pos += 1;
            self.last_span = Span::new(whole.start, mid);
            return true;
        }
        false
    }

    /// Like `eat_close_angle`, but records an `Expected` error (without
    /// consuming anything) if neither a `>` nor a splittable `>>` is here.
    pub fn expect_close_angle(&mut self, expected: &'static str) -> bool {
        if self.eat_close_angle() {
            true
        } else {
            self.error(ParseErrorKind::Expected {
                expected,
                found: self.peek().describe(),
            });
            false
        }
    }

    /// Consumes the current token if it's an `Ident`, returning its name;
    /// otherwise records an `Expected` error and returns `None`.
    pub fn expect_ident_with_origin(
        &mut self,
    ) -> Option<(
        crate::ast::identifier::Ident,
        crate::ast::identifier::Origin,
    )> {
        if let TokenKind::Ident(name) = self.peek() {
            let name = name.clone();
            let origin = self.advance().origin;
            Some((crate::ast::identifier::Ident(name), origin))
        } else {
            self.error(ParseErrorKind::Expected {
                expected: "an identifier",
                found: self.peek().describe(),
            });
            None
        }
    }

    pub fn expect_ident(&mut self) -> Option<crate::ast::identifier::Ident> {
        self.expect_ident_with_origin().map(|(ident, _)| ident)
    }

    pub fn error(&mut self, kind: ParseErrorKind) {
        self.error_at(self.peek_span(), kind);
    }

    pub fn error_at(&mut self, span: Span, kind: ParseErrorKind) {
        self.errors.push(ParseError::new(span, kind));
    }
}

/// A binding declaration's leading `mut`/`comp` flags, once the *whole*
/// `[mut] [comp] ident (':='|':')` shape has been confirmed and the
/// modifiers consumed.
///
/// Both words are contextual keywords: they lead a binding here and are
/// ordinary identifiers everywhere else, so nothing is consumed until the
/// full shape matches -- the commit rule described in `parser::contextual`.
///
/// `mut comp x := ...` parses with both flags set and is rejected later by
/// analysis (`AnalysisErrorKind::MutCompBinding`), not here.
#[derive(Clone, Copy)]
pub struct BindingPrefix {
    pub mutable: bool,
    pub comp: bool,
}

/// Consumes a leading `mut`/`comp` run if -- and only if -- a binding
/// declaration genuinely follows it. Returns `None` (having consumed
/// nothing) otherwise, including for a binding written with no modifiers at
/// all, which both callers handle on their ordinary paths.
///
/// Shared by item and statement position, which had two verbatim copies of
/// this lookahead and two near-identical explanations of it.
pub fn parse_binding_prefix(p: &mut Parser) -> Option<BindingPrefix> {
    let mut_offset = usize::from(p.at_contextual(contextual::MUT));
    let comp_offset = usize::from(p.at_contextual_at(mut_offset, contextual::COMP));
    if mut_offset + comp_offset == 0 {
        return None;
    }
    let ident_offset = mut_offset + comp_offset;
    if !matches!(p.peek_at(ident_offset), TokenKind::Ident(_))
        || !matches!(
            p.peek_at(ident_offset + 1),
            TokenKind::ColonEq | TokenKind::Colon
        )
    {
        return None;
    }
    for _ in 0..ident_offset {
        p.advance();
    }
    Some(BindingPrefix {
        mutable: mut_offset > 0,
        comp: comp_offset > 0,
    })
}

/// `a`, or `a::b::c` -- shared by type position, expression position, and
/// `import` statements alike (see `ast::identifier::Path`'s own doc
/// comment). `::` is matched as one atomic token by the lexer already
/// (maximal munch), so there's no risk of it being mistaken for two bare
/// `:`s here.
pub fn parse_path(p: &mut Parser) -> Option<crate::ast::identifier::Path> {
    let (head, origin) = p.expect_ident_with_origin()?;
    let mut tail = Vec::new();
    while p.check(&TokenKind::ColonColon) {
        p.advance();
        match p.expect_ident() {
            Some(seg) => tail.push(seg),
            None => break,
        }
    }
    Some(crate::ast::identifier::Path { head, tail, origin })
}

/// Zero or more `name: Type` parameters, comma-separated.
///
/// A comma is only consumed when another parameter actually follows, so a
/// trailing comma before `)` is left for the caller to reject rather than
/// silently swallowed.
///
/// One production, shared by real definitions (`parser::item`) and
/// function *types* (`parser::type`), which previously had two
/// character-for-character identical copies differing only in element type.
pub fn parse_param_decls(p: &mut Parser) -> Vec<crate::ast::r#type::Param> {
    let mut params = Vec::new();
    if !matches!(p.peek(), TokenKind::Ident(_)) {
        return params;
    }
    while let Some(param) = parse_param_decl(p) {
        params.push(param);
        if matches!(p.peek(), TokenKind::Comma) && matches!(p.peek_at(1), TokenKind::Ident(_)) {
            p.advance();
        } else {
            break;
        }
    }
    params
}

fn parse_param_decl(p: &mut Parser) -> Option<crate::ast::r#type::Param> {
    let (ident, origin) = p.expect_ident_with_origin()?;
    let name_span = p.last_span();
    p.expect(&TokenKind::Colon, "':'");
    let r#type = crate::parser::r#type::parse_type(p)?;
    Some(crate::ast::r#type::Param {
        ident,
        name_span,
        span: name_span.to(p.last_span()),
        origin,
        r#type,
    })
}

/// `self` / `mut self` / `*self` / `*mut self` (optionally followed by
/// `, name: Type, ...`), or just `name: Type, ...` -- see `parse_self_mode`.
pub fn parse_param_list(p: &mut Parser) -> (Option<crate::ast::self_mode::SelfMode>, Vec<crate::ast::r#type::Param>) {
    match parse_self_mode(p) {
        Some(mode) => {
            let rest = if p.eat(&TokenKind::Comma) { parse_param_decls(p) } else { Vec::new() };
            (Some(mode), rest)
        }
        None => (None, parse_param_decls(p)),
    }
}

/// `self` / `mut self` / `*self` / `*mut self` -- the four ways a member
/// function's `self` parameter can be spelled, shared by both parameter-list
/// parsers (`parser::item::parse_param_list` for real function/method
/// definitions, `parser::type::parse_param_list` for member-function type
/// annotations). Returns `None` (consuming nothing) if what follows isn't
/// one of these four shapes, so callers fall through to ordinary
/// parameter-list parsing untouched.
pub fn parse_self_mode(p: &mut Parser) -> Option<crate::ast::self_mode::SelfMode> {
    use crate::ast::self_mode::SelfMode;
    use crate::parser::contextual::{MUT, SELF};

    // '*' can never legally start an ordinary `ident: Type` parameter, so
    // eating it here is unambiguous -- but once eaten, only `self`/`mut
    // self` may follow; anything else is a specific parse error rather
    // than a generic "expected an identifier" later.
    let by_pointer = p.eat(&TokenKind::Star);
    // `mut` must NOT be eaten until `self` is confirmed at peek_at(1),
    // since `mut` is a legal ordinary identifier everywhere outside this
    // exact position.
    let mutable = if p.at_contextual(MUT) && p.at_contextual_at(1, SELF) {
        p.advance(); // 'mut'
        true
    } else {
        false
    };
    if p.eat_contextual(SELF) {
        return Some(match (by_pointer, mutable) {
            (false, false) => SelfMode::Value,
            (false, true) => SelfMode::MutValue,
            (true, false) => SelfMode::Pointer,
            (true, true) => SelfMode::MutPointer,
        });
    }
    if by_pointer {
        p.error(ParseErrorKind::Expected {
            expected: "'self' after '*'/'*mut'",
            found: p.peek().describe(),
        });
    }
    None
}
