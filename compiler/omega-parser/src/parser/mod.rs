pub mod contextual;
mod cursor;
pub mod expression;
pub mod item;
mod legacy;
pub mod macro_syntax;
pub mod recovery;
pub mod statement;
pub mod r#type;

use crate::ast::identifier::Origin;
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

    pub fn errors(&self) -> &[ParseError] {
        &self.errors
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

    /// The provenance of the token about to be consumed. Node constructors
    /// use the token that introduces a construct as that construct's syntax
    /// owner, so macro-authored syntax stays distinguishable from the
    /// caller-written syntax substituted into it.
    pub fn peek_origin(&self) -> Origin {
        self.cursor.peek_origin()
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    pub fn advance(&mut self) -> Token {
        self.cursor.advance()
    }

    /// How far the token stream has been consumed. Recovery compares it to
    /// prove a step made progress rather than looping on the same token.
    pub fn position(&self) -> usize {
        self.cursor.position()
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

    /// Whether any diagnostic was recorded after `mark`. Speculative parses
    /// use this to tell "this really parsed" from "this parsed with recovery".
    pub fn errors_since(&self, mark: &Mark) -> bool {
        self.errors.len() > mark.error_count
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
        } else if let Some(keyword) = self.peek().reserved_word() {
            // Taken as the name so the rest of the declaration still parses:
            // the error already fails the compilation.
            self.error(ParseErrorKind::KeywordAsName { keyword });
            let origin = self.advance().origin;
            Some((crate::ast::identifier::Ident(keyword.into()), origin))
        } else {
            self.error(ParseErrorKind::Expected {
                expected: "an identifier",
                found: self.peek().describe(),
            });
            None
        }
    }

    /// Whether a declared name begins at `offset`: an identifier, or a
    /// reserved keyword directly before a declaration's `:`/`:=`, which
    /// `expect_ident` then reports as a keyword used as a name.
    pub fn declared_name_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            TokenKind::Ident(_) => true,
            kind => {
                kind.reserved_word().is_some()
                    && matches!(
                        self.peek_at(offset + 1),
                        TokenKind::Colon | TokenKind::ColonEq
                    )
            }
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

/// Whether a `name := ...` / `name : T` binding (with optional `comp`/`^`)
/// begins here, without consuming the modifiers.
pub fn binding_modifiers_follow(p: &Parser) -> bool {
    binding_prefix_here(p).is_some()
}

pub fn parse_binding_modifiers(p: &mut Parser) -> Option<BindingModifiers> {
    let found = binding_prefix_here(p)?;
    for offset in 0..found.len {
        if found.legacy_mut == Some(offset) {
            p.error(ParseErrorKind::LegacyMutBinding);
        }
        p.advance();
    }
    Some(found.prefix)
}

struct FoundBindingPrefix {
    prefix: BindingPrefix,
    len: usize,
    legacy_mut: Option<usize>,
}

/// The modifiers of a binding that begins here, if one does. A `comp` with no
/// name after it is the name itself (`comp := 5`), so that it is reported as a
/// keyword used as a name rather than as a malformed `comp` expression. A
/// retired `mut` (`mut x`, `mut comp x`, `comp mut x`) is recognised so that
/// it can be reported, and stands for `^`.
fn binding_prefix_here(p: &Parser) -> Option<FoundBindingPrefix> {
    let retired_mut_at = |offset| legacy::mut_at(p, offset) && !declaration_follows(p, offset);
    let mut legacy_mut = retired_mut_at(0).then_some(0);
    let mut len = usize::from(legacy_mut.is_some());
    let comp = matches!(p.peek_at(len), TokenKind::Comp) && !declaration_follows(p, len);
    len += usize::from(comp);
    let caret = p.peek_at(len) == &TokenKind::Caret;
    if caret {
        len += 1;
    } else if legacy_mut.is_none() && retired_mut_at(len) {
        legacy_mut = Some(len);
        len += 1;
    }
    let mutable = caret || legacy_mut.is_some();
    declaration_follows(p, len).then_some(FoundBindingPrefix {
        prefix: BindingPrefix { mutable, comp },
        len,
        legacy_mut,
    })
}

fn declaration_follows(p: &Parser, offset: usize) -> bool {
    p.declared_name_at(offset)
        && matches!(p.peek_at(offset + 1), TokenKind::ColonEq | TokenKind::Colon)
}

pub fn parse_binding_prefix(p: &mut Parser) -> Option<BindingPrefix> {
    parse_binding_modifiers(p)
}

/// Parses a leading explicit path anchor (`root::`, `self::`, one or more
/// chained `super::`) if present. `root`, `self`, and `super` are contextual:
/// only a leading anchored form followed by `::` is navigation, so this must
/// look ahead before committing.
pub(crate) fn parse_path_anchor(p: &mut Parser) -> Option<crate::ast::identifier::PathAnchor> {
    use crate::ast::identifier::PathAnchor;
    if p.at_contextual(contextual::ROOT) && matches!(p.peek_at(1), TokenKind::ColonColon) {
        p.advance();
        p.advance();
        Some(PathAnchor::Root)
    } else if p.at_contextual(contextual::SELF) && matches!(p.peek_at(1), TokenKind::ColonColon) {
        p.advance();
        p.advance();
        Some(PathAnchor::SelfModule)
    } else if p.at_contextual(contextual::SUPER) && matches!(p.peek_at(1), TokenKind::ColonColon) {
        let mut depth = 0u32;
        while p.at_contextual(contextual::SUPER) && matches!(p.peek_at(1), TokenKind::ColonColon) {
            p.advance();
            p.advance();
            depth += 1;
        }
        Some(PathAnchor::Super(depth))
    } else {
        None
    }
}

pub fn parse_path(p: &mut Parser) -> Option<crate::ast::identifier::Path> {
    let anchor = parse_path_anchor(p);
    let (head, origin) = p.expect_ident_with_origin()?;
    let mut tail = Vec::new();
    while p.check(&TokenKind::ColonColon) {
        p.advance();
        match p.expect_ident() {
            Some(seg) => tail.push(seg),
            None => break,
        }
    }
    Some(crate::ast::identifier::Path {
        anchor,
        head,
        tail,
        origin,
    })
}

pub fn parse_param_decls(p: &mut Parser) -> Vec<crate::ast::r#type::Param> {
    let mut params = Vec::new();
    if !param_decl_at(p, 0) {
        return params;
    }
    while let Some(param) = parse_param_decl(p) {
        params.push(param);
        if matches!(p.peek(), TokenKind::Comma) && param_decl_at(p, 1) {
            p.advance();
        } else {
            break;
        }
    }
    params
}

fn param_decl_at(p: &Parser, offset: usize) -> bool {
    let marker = usize::from(p.peek_at(offset) == &TokenKind::Caret);
    p.declared_name_at(offset + marker)
}

fn parse_param_decl(p: &mut Parser) -> Option<crate::ast::r#type::Param> {
    let start = p.peek_span();
    let mutable = p.eat(&TokenKind::Caret)
        || legacy::eat_mut_before(p, |p, offset| {
            p.declared_name_at(offset) && p.peek_at(offset + 1) == &TokenKind::Colon
        });
    let (ident, origin) = p.expect_ident_with_origin()?;
    let name_span = p.last_span();
    p.expect(&TokenKind::Colon, "':'");
    let r#type = crate::parser::r#type::parse_type(p)?;
    Some(crate::ast::r#type::Param {
        ident,
        name_span,
        span: start.to(p.last_span()),
        origin,
        r#type,
        mutable,
    })
}

/// `^` declares a mutable local of the function's body, so a declaration
/// without one cannot carry it.
pub fn reject_mutable_params(
    p: &mut Parser,
    params: &[crate::ast::r#type::Param],
    context: &'static str,
) {
    for param in params.iter().filter(|param| param.mutable) {
        p.error_at(
            Span::new(param.span.start, param.name_span.end),
            ParseErrorKind::MutableParamWithoutBody { context },
        );
    }
}

pub fn parse_param_list(
    p: &mut Parser,
) -> (
    Option<crate::ast::self_mode::SelfParam>,
    Vec<crate::ast::r#type::Param>,
) {
    match parse_self_param(p) {
        Some(receiver) => {
            let rest = if p.eat(&TokenKind::Comma) {
                parse_param_decls(p)
            } else {
                Vec::new()
            };
            (Some(receiver), rest)
        }
        None => (None, parse_param_decls(p)),
    }
}

/// `self`, `*self`, or `^self`. The pointer receivers mirror the address a
/// call site takes, so `^self` is `self : ^Self` -- the one place where `^`
/// before a parameter name does not declare a mutable binding.
pub fn parse_self_param(p: &mut Parser) -> Option<crate::ast::self_mode::SelfParam> {
    use crate::ast::self_mode::{SelfMode, SelfParam};
    use crate::parser::contextual::SELF;

    let start = p.peek_span();
    let pointer_mutable = match p.peek() {
        TokenKind::Star if p.at_contextual_at(1, SELF) || legacy::mut_self_at(p, 1) => Some(false),
        TokenKind::Caret if p.at_contextual_at(1, SELF) => Some(true),
        _ => None,
    };
    let legacy_mut = legacy::mut_self_at(p, usize::from(pointer_mutable.is_some()));
    if pointer_mutable.is_none() && !legacy_mut && !p.at_contextual(SELF) {
        // A leading `*` can only begin a receiver: parameters are `name : T`.
        if p.eat(&TokenKind::Star) {
            p.error(ParseErrorKind::Expected {
                expected: "'self' after '*'",
                found: p.peek().describe(),
            });
        }
        return None;
    }
    if pointer_mutable.is_some() {
        p.advance();
    }
    if legacy_mut {
        p.advance(); // 'mut'
    }
    p.advance(); // 'self'
    let span = p.last_span();
    let mode = match (pointer_mutable, legacy_mut) {
        (None, false) => SelfMode::Value,
        (None, true) => {
            p.error_at(start.to(span), ParseErrorKind::LegacyMutSelf);
            SelfMode::Value
        }
        (Some(false), false) => SelfMode::Pointer,
        (Some(false), true) => {
            p.error_at(
                start.to(span),
                ParseErrorKind::LegacyMutPointer { receiver: true },
            );
            SelfMode::MutPointer
        }
        (Some(true), _) => SelfMode::MutPointer,
    };
    Some(SelfParam { mode, span })
}
