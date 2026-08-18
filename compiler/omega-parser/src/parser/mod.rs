pub mod contextual;
pub mod expression;
pub mod item;
pub mod macro_syntax;
pub mod recovery;
pub mod statement;
pub mod r#type;

use crate::diagnostics::{ParseError, ParseErrorKind, Span};
use crate::lexer::{Token, TokenKind};

pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    errors: Vec<ParseError>,
    struct_literals_restricted: bool,
    pending_gt: Option<Span>,
    last_span: Span,
    depth: usize,
    depth_exceeded: bool,
}

pub const MAX_NESTING_DEPTH: usize = 256;

const CLOSE_ANGLE_KIND: TokenKind = TokenKind::Gt;

#[derive(Clone, Copy)]
pub struct Mark {
    pos: usize,
    error_count: usize,
    pending_gt: Option<Span>,
    last_span: Span,
}

impl<'a> Parser<'a> {
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

    pub fn struct_literals_allowed(&self) -> bool {
        !self.struct_literals_restricted
    }

    pub fn restrict_struct_literals<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let previous = std::mem::replace(&mut self.struct_literals_restricted, true);
        let result = f(self);
        self.struct_literals_restricted = previous;
        result
    }

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

    pub fn last_span(&self) -> Span {
        self.last_span
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

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

    pub fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

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

#[derive(Clone, Copy)]
pub struct BindingPrefix {
    pub mutable: bool,
    pub comp: bool,
}

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

pub fn parse_param_list(p: &mut Parser) -> (Option<crate::ast::self_mode::SelfMode>, Vec<crate::ast::r#type::Param>) {
    match parse_self_mode(p) {
        Some(mode) => {
            let rest = if p.eat(&TokenKind::Comma) { parse_param_decls(p) } else { Vec::new() };
            (Some(mode), rest)
        }
        None => (None, parse_param_decls(p)),
    }
}

pub fn parse_self_mode(p: &mut Parser) -> Option<crate::ast::self_mode::SelfMode> {
    use crate::ast::self_mode::SelfMode;
    use crate::parser::contextual::{MUT, SELF};

    // Leading `*` uniquely commits to pointer-`self`; reject any other continuation precisely.
    let by_pointer = p.eat(&TokenKind::Star);
    // Do not consume contextual `mut` until the following `self` confirms this form.
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
