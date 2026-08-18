use crate::ast::expression::NumberBase;
use crate::ast::r#type::{FunctionType, Type};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, contextual, parse_path};

/// `*T` / `[N]T` / `[]T` / `[?]T` / `(params) => T` / `Path` / `Path<T,
/// ...>` -- no ambiguity between them, each starts with a distinct token.
/// Mutability is never parsed here -- it only ever appears right after a
/// leading `*` (`parse_pointer_type`), so `*[]T`/`*mut []T`/... all fall out
/// of the ordinary pointer grammar recursing back into this function.
///
/// The second of the grammar's two cycles (the other is `parse_expression`),
/// and so the second `descend` site: `*[?]*[?]T` recurses entirely within
/// types and never passes through expression parsing, so the expression
/// guard alone would not bound it. Both share one counter -- see
/// `Parser::descend`.
pub fn parse_type(p: &mut Parser) -> Option<Type> {
    p.descend(|p| match p.peek() {
        TokenKind::Star => parse_pointer_type(p),
        TokenKind::LBracket => parse_bracket_type(p),
        TokenKind::LParen => parse_function_type(p),
        TokenKind::Spec => parse_spec_object_type(p),
        TokenKind::Ident(_) => parse_named_type(p),
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "a type",
                found: p.peek().describe(),
            });
            None
        }
    })
}

/// `*T` or `*mut T` -- `mut` is a contextual keyword here (see
/// `lexer::TokenKind`'s doc comment on why it's not a global one, exactly
/// like `self`), checked by comparing an already-lexed `Ident`'s text.
fn parse_pointer_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '*'
    let mutable = p.eat_contextual(contextual::MUT);
    let inner = parse_type(p)?;
    Some(Type::Pointer(Box::new(inner), mutable))
}

/// `spec *Animal` / `spec *mut Animal` (dynamic dispatch, `Type::SpecObject`)
/// or `spec Animal` (static dispatch, `Type::SpecStatic`, no `*` at all) --
/// disambiguated purely by whether a `*` immediately follows `spec`. Both
/// forms parse their pointee via the same named-type path (`Path` or
/// `Path<...>`), never recursing back into `parse_type` -- a spec
/// reference is always a plain name, never itself a pointer.
fn parse_spec_object_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // 'spec'
    if !p.eat(&TokenKind::Star) {
        let inner = parse_named_type(p)?;
        return Some(Type::SpecStatic(Box::new(inner)));
    }
    let mutable = p.eat_contextual(contextual::MUT);
    let inner = parse_named_type(p)?;
    Some(Type::SpecObject(Box::new(inner), mutable))
}

/// `[]T` / `[?]T` / `[N]T` -- the three bracketed-array shapes,
/// disambiguated by what immediately follows `[`: a closing `]` right away
/// means unsized, a `?` means unknown-size, anything else must be a
/// decimal size. In every case the item type is parsed *last*, after the
/// closing `]` -- unlike the old `[T; N]` grammar, the size (or its
/// absence/placeholder) always comes first.
fn parse_bracket_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '['
    match p.peek() {
        TokenKind::RBracket => {
            p.advance();
            let item = parse_type(p)?;
            Some(Type::InferredArray(Box::new(item)))
        }
        TokenKind::Question => {
            p.advance();
            p.expect(&TokenKind::RBracket, "']'");
            let item = parse_type(p)?;
            Some(Type::UnknownSizeArray(Box::new(item)))
        }
        _ => {
            let size = parse_array_size(p)?;
            p.expect(&TokenKind::RBracket, "']'");
            let item = parse_type(p)?;
            Some(Type::SizedArray(Box::new(item), size))
        }
    }
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
            if matches!(n.base, NumberBase::Decimal) && n.explicit_type.is_none() =>
        {
            let size = n.integer_part.clone();
            p.advance();
            Some(size)
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "an array size",
                found: p.peek().describe(),
            });
            None
        }
    }
}

fn parse_function_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '('
    let (self_mode, params) = crate::parser::parse_param_list(p);
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
        p.expect_close_angle("'>'");
        Some(Type::Generic(path, args))
    } else {
        Some(Type::Named(path))
    }
}
