pub mod contextual;
pub mod expression;
pub mod item;
pub mod macro_syntax;
pub mod recovery;
pub mod statement;
pub mod r#type;
mod cursor;

use crate::diagnostics::{ParseError, ParseErrorKind, Span};
use crate::lexer::{Token, TokenKind};

use cursor::{CursorMark, TokenCursor};

pub struct Parser<'a> {
    cursor: TokenCursor<'a>,
    errors: Vec<ParseError>,
    struct_literals_restricted: bool,
    depth: usize,
    depth_exceeded: bool,
}

pub const MAX_NESTING_DEPTH: usize = 256;

#[derive(Clone, Copy)]
pub struct Mark {
    cursor: CursorMark,
    error_count: usize,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self {
            cursor: TokenCursor::new(tokens),
            errors: Vec::new(),
            struct_literals_restricted: false,
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

    fn with_struct_literals_restricted<T>(
        &mut self,
        restricted: bool,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.struct_literals_restricted, restricted);
        let result = f(self);
        self.struct_literals_restricted = previous;
        result
    }

    pub fn restrict_struct_literals<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.with_struct_literals_restricted(true, f)
    }

    pub fn allow_struct_literals<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.with_struct_literals_restricted(false, f)
    }

    pub fn into_errors(self) -> Vec<ParseError> {
        self.errors
    }

    pub fn peek(&self) -> &TokenKind {
        self.cursor.peek()
    }

    pub fn peek_at(&self, offset: usize) -> &TokenKind {
        self.cursor.peek_at(offset)
    }

    pub fn peek_span(&self) -> Span {
        self.cursor.peek_span()
    }

    pub fn last_span(&self) -> Span {
        self.cursor.last_span()
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    pub fn advance(&mut self) -> Token {
        self.cursor.advance()
    }

    pub fn mark(&self) -> Mark {
        Mark {
            cursor: self.cursor.mark(),
            error_count: self.errors.len(),
        }
    }

    pub fn reset(&mut self, mark: Mark) {
        self.cursor.reset(mark.cursor);
        self.errors.truncate(mark.error_count);
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
        self.cursor.eat_close_angle()
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

#[derive(Clone, Copy, Default)]
pub struct BindingPrefix {
    pub mutable: bool,
    pub comp: bool,
}

pub type BindingModifiers = BindingPrefix;

pub fn parse_binding_modifiers(p: &mut Parser) -> Option<BindingModifiers> {
    if matches!(p.peek(), TokenKind::Ident(_))
        && matches!(p.peek_at(1), TokenKind::ColonEq | TokenKind::Colon)
    {
        return Some(BindingPrefix::default());
    }

    let mutable = p.at_contextual(contextual::MUT);
    let comp_offset = usize::from(mutable);
    let comp = p.at_contextual_at(comp_offset, contextual::COMP);
    let modifier_count = usize::from(mutable) + usize::from(comp);

    if !matches!(p.peek_at(modifier_count), TokenKind::Ident(_))
        || !matches!(
            p.peek_at(modifier_count + 1),
            TokenKind::ColonEq | TokenKind::Colon
        )
    {
        return None;
    }

    for _ in 0..modifier_count {
        p.advance();
    }
    Some(BindingPrefix { mutable, comp })
}

pub fn parse_binding_prefix(p: &mut Parser) -> Option<BindingPrefix> {
    parse_binding_modifiers(p)
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
