use crate::ast::identifier::Ident;
use crate::ast::r#type::{FunctionType, Type};
use crate::ast::self_mode::SelfMode;
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, parse_path};

/// `*T` / `[T]` / `[T; N]` / `(params) => T` / `Path` / `Path<T, ...>` --
/// matches `Type::parser`'s original `choice((pointer, array, function,
/// named))` dispatch order exactly (no ambiguity between them: each starts
/// with a distinct token).
pub fn parse_type(p: &mut Parser) -> Option<Type> {
    match p.peek() {
        TokenKind::Star => parse_pointer_type(p),
        TokenKind::LBracket => parse_array_type(p),
        TokenKind::LParen => parse_function_type(p),
        TokenKind::Spec => parse_spec_object_type(p),
        TokenKind::Ident(_) => parse_named_type(p),
        _ => {
            p.error(ParseErrorKind::Expected { expected: "a type", found: p.peek().describe() });
            None
        }
    }
}

/// `*T` or `*mut T` -- `mut` is a contextual keyword here (see
/// `lexer::TokenKind`'s doc comment on why it's not a global one, exactly
/// like `self`), checked by comparing an already-lexed `Ident`'s text.
fn parse_pointer_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '*'
    let mutable = if let TokenKind::Ident(name) = p.peek()
        && name == "mut"
    {
        p.advance();
        true
    } else {
        false
    };
    let inner = parse_type(p)?;
    Some(Type::Pointer(Box::new(inner), mutable))
}

/// `spec *Animal` or `spec *mut Animal` -- mirrors `parse_pointer_type`
/// exactly (same `mut` contextual-keyword handling), just requiring a `*`
/// to immediately follow `spec` and producing `Type::SpecObject` instead of
/// `Type::Pointer`. The pointee is parsed via the ordinary named-type path
/// (`Path` or `Path<...>`), never recursing back into `parse_type` -- a
/// spec object's pointee is always a spec reference, never another pointer.
fn parse_spec_object_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // 'spec'
    p.expect(&TokenKind::Star, "'*'");
    let mutable = if let TokenKind::Ident(name) = p.peek()
        && name == "mut"
    {
        p.advance();
        true
    } else {
        false
    };
    let inner = parse_named_type(p)?;
    Some(Type::SpecObject(Box::new(inner), mutable))
}

fn parse_array_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '['
    let inner = parse_type(p)?;
    let ty = if p.eat(&TokenKind::Semi) {
        let size = parse_array_size(p)?;
        Type::SizedArray(Box::new(inner), size)
    } else {
        Type::Array(Box::new(inner))
    };
    p.expect(&TokenKind::RBracket, "']'");
    Some(ty)
}

/// A sized array's `N` is kept as raw digit text, matching `NumberExpr`'s
/// own "parser never rejects on value, only shape" philosophy -- but unlike
/// an ordinary number *expression*, it must be a bare decimal integer with
/// no separators/suffix/fraction (the old grammar parsed this with its own
/// narrower `text::digits(10)` rule, entirely independent of
/// `NumberExpr::parser`), so a based/suffixed/fractional literal here is
/// rejected rather than silently accepted with a misleading size string.
fn parse_array_size(p: &mut Parser) -> Option<String> {
    match p.peek() {
        TokenKind::Number(n)
            if matches!(n.base, crate::ast::expression::number::NumberBase::Decimal)
                && n.fractional_part.is_none()
                && n.explicit_type.is_none() =>
        {
            let size = n.integer_part.clone();
            p.advance();
            Some(size)
        }
        _ => {
            p.error(ParseErrorKind::Expected { expected: "an array size", found: p.peek().describe() });
            None
        }
    }
}

fn parse_function_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '('
    let (self_mode, params) = parse_param_list(p);
    let is_variadic = if p.eat(&TokenKind::Comma) {
        p.expect(&TokenKind::DotDotDot, "'...'");
        true
    } else {
        false
    };
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type = parse_type(p)?;
    Some(Type::Function(FunctionType {
        params,
        return_type: Box::new(return_type),
        is_variadic,
        self_mode,
    }))
}

/// `self` / `mut self` / `*self` / `*mut self` (optionally followed by `,
/// ident: Type, ...`), or just `ident: Type, ...` -- see
/// `crate::parser::parse_self_mode`.
fn parse_param_list(p: &mut Parser) -> (Option<SelfMode>, Vec<(Ident, Type)>) {
    match crate::parser::parse_self_mode(p) {
        Some(mode) => {
            let rest = if p.eat(&TokenKind::Comma) { parse_decl_list(p) } else { Vec::new() };
            (Some(mode), rest)
        }
        None => (None, parse_decl_list(p)),
    }
}

/// Zero or more `ident: Type` pairs, comma-separated -- a comma is only
/// consumed if another decl actually follows, so a trailing comma before
/// `)` is left unconsumed (a real parse error at the caller, matching the
/// old grammar's plain `separated_by` -- which, without `.allow_trailing()`,
/// doesn't tolerate one either) rather than silently swallowed.
fn parse_decl_list(p: &mut Parser) -> Vec<(Ident, Type)> {
    let mut decls = Vec::new();
    if !matches!(p.peek(), TokenKind::Ident(_)) {
        return decls;
    }
    while let Some(decl) = parse_type_decl(p) {
        decls.push(decl);
        if matches!(p.peek(), TokenKind::Comma) && matches!(p.peek_at(1), TokenKind::Ident(_)) {
            p.advance();
        } else {
            break;
        }
    }
    decls
}

fn parse_type_decl(p: &mut Parser) -> Option<(Ident, Type)> {
    let ident = p.expect_ident()?;
    p.expect(&TokenKind::Colon, "':'");
    let ty = parse_type(p)?;
    Some((ident, ty))
}

/// `Path`, or `Path<Type, ...>` -- `<` is unambiguous here: this grammar has
/// no comparison/expression operators at all in type position, so it can
/// only ever mean "generic arguments follow."
fn parse_named_type(p: &mut Parser) -> Option<Type> {
    let path = parse_path(p)?;
    if p.eat(&TokenKind::Lt) {
        let mut args = vec![parse_type(p)?];
        while p.eat(&TokenKind::Comma) {
            args.push(parse_type(p)?);
        }
        p.expect(&TokenKind::Gt, "'>'");
        Some(Type::Generic(path, args))
    } else {
        Some(Type::Named(path))
    }
}
