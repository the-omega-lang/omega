use crate::ast::identifier::Origin;
use crate::diagnostics::Span;
use crate::lexer::{Token, TokenKind};

const CLOSE_ANGLE_KIND: TokenKind = TokenKind::Gt;

pub(super) struct TokenCursor<'a> {
    tokens: &'a [Token],
    pos: usize,
    pending_gt: Option<Span>,
    last_span: Span,
}

#[derive(Clone, Copy)]
pub(super) struct CursorMark {
    pos: usize,
    pending_gt: Option<Span>,
    last_span: Span,
}

impl<'a> TokenCursor<'a> {
    pub(super) fn new(tokens: &'a [Token]) -> Self {
        assert!(
            matches!(tokens.last().map(|token| &token.kind), Some(TokenKind::Eof)),
            "parser token streams must end with EOF",
        );
        Self {
            tokens,
            pos: 0,
            pending_gt: None,
            last_span: Span::new(0, 0),
        }
    }

    pub(super) fn peek(&self) -> &TokenKind {
        if self.pending_gt.is_some() {
            &CLOSE_ANGLE_KIND
        } else {
            &self.tokens[self.pos].kind
        }
    }

    pub(super) fn peek_at(&self, offset: usize) -> &TokenKind {
        if self.pending_gt.is_some() {
            if offset == 0 {
                return &CLOSE_ANGLE_KIND;
            }
            let index = (self.pos + offset - 1).min(self.tokens.len() - 1);
            return &self.tokens[index].kind;
        }
        let index = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[index].kind
    }

    pub(super) fn peek_span(&self) -> Span {
        self.pending_gt
            .unwrap_or_else(|| self.tokens[self.pos].span)
    }

    pub(super) fn peek_origin(&self) -> Origin {
        if self.pending_gt.is_some() {
            return Origin::default();
        }
        self.tokens[self.pos].origin
    }

    pub(super) fn last_span(&self) -> Span {
        self.last_span
    }

    pub(super) fn advance(&mut self) -> Token {
        if let Some(span) = self.pending_gt.take() {
            self.last_span = span;
            return Token {
                kind: TokenKind::Gt,
                span,
                origin: Origin::default(),
            };
        }

        let token = self.tokens[self.pos].clone();
        if !matches!(token.kind, TokenKind::Eof) {
            self.pos += 1;
            self.last_span = token.span;
        }
        token
    }

    pub(super) fn position(&self) -> usize {
        self.pos
    }

    pub(super) fn mark(&self) -> CursorMark {
        CursorMark {
            pos: self.pos,
            pending_gt: self.pending_gt,
            last_span: self.last_span,
        }
    }

    pub(super) fn reset(&mut self, mark: CursorMark) {
        self.pos = mark.pos;
        self.pending_gt = mark.pending_gt;
        self.last_span = mark.last_span;
    }

    pub(super) fn eat_close_angle(&mut self) -> bool {
        if matches!(self.peek(), TokenKind::Gt) {
            self.advance();
            return true;
        }
        if !matches!(self.peek(), TokenKind::Shr) {
            return false;
        }

        let whole = self.peek_span();
        let middle = whole.start + 1;
        self.pending_gt = Some(Span::new(middle, whole.end));
        self.pos += 1;
        self.last_span = Span::new(whole.start, middle);
        true
    }
}
