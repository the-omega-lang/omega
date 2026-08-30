use crate::ast::self_mode::SelfMode;
use crate::ast::r#type::{
    ArrayLength, CompLiteral, FunctionType, FunctionTypeParam, GenericArg, RawConvention, Type,
};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, contextual, parse_path};

pub fn parse_type(p: &mut Parser) -> Option<Type> {
    p.descend(|p| match p.peek() {
        TokenKind::Star => parse_pointer_type(p),
        TokenKind::LBracket => parse_bracket_type(p),
        TokenKind::LParen => parse_function_type(p),
        TokenKind::Spec => parse_spec_static_type(p),
        TokenKind::Enum => parse_anonymous_enum_type(p),
        TokenKind::Foreign => parse_foreign_function_type(p),
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

fn parse_pointer_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '*'
    let mutable = p.eat_contextual(contextual::MUT);
    let inner = parse_type(p)?;
    Some(Type::Pointer(Box::new(inner), mutable))
}

fn parse_spec_static_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // 'spec'
    let mut members = vec![parse_named_type(p)?];
    while p.eat(&TokenKind::Plus) {
        members.push(parse_named_type(p)?);
    }
    Some(Type::SpecStatic(members))
}

/// `enum A | B | ...`. Members are full types, so `|` is consumed only by
/// this production -- a member's own type syntax never contains a top-level
/// `|`, and the list therefore ends at the first token that cannot continue a
/// type.
fn parse_anonymous_enum_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // 'enum'
    let mut members = vec![parse_type(p)?];
    while p.eat(&TokenKind::Pipe) {
        members.push(parse_type(p)?);
    }
    Some(Type::AnonymousEnum(members))
}

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
            let length = parse_array_length(p)?;
            p.expect(&TokenKind::RBracket, "']'");
            let item = parse_type(p)?;
            Some(Type::SizedArray(Box::new(item), length))
        }
    }
}

/// A fixed array's length. A literal is checked here; a path is left for
/// semantic resolution, which is the only layer that knows whether it names
/// a `comp` binding and what its value is.
fn parse_array_length(p: &mut Parser) -> Option<ArrayLength> {
    if matches!(p.peek(), TokenKind::Ident(_)) {
        return Some(ArrayLength::Path(parse_path(p)?));
    }
    match parse_comp_literal(p) {
        Some(literal) => Some(ArrayLength::Literal(literal)),
        None => {
            p.error(ParseErrorKind::Expected {
                expected: "an array length",
                found: p.peek().describe(),
            });
            None
        }
    }
}

/// One scalar compile-time literal. Returns `None` without reporting when the
/// next token cannot start one, so callers that also accept type syntax can
/// fall through.
fn parse_comp_literal(p: &mut Parser) -> Option<CompLiteral> {
    let negative = p.check(&TokenKind::Minus);
    let literal = match p.peek_at(usize::from(negative)) {
        TokenKind::Number(n) if n.fractional_part.is_none() => CompLiteral::Int {
            negative,
            number: n.clone(),
        },
        TokenKind::True if !negative => CompLiteral::Bool(true),
        TokenKind::False if !negative => CompLiteral::Bool(false),
        TokenKind::Char(c) if !negative => CompLiteral::Char(*c),
        _ => return None,
    };
    if negative {
        p.advance();
    }
    p.advance();
    Some(literal)
}

/// One written generic argument. A scalar literal is a value; everything
/// else -- including a bare path -- is kept as type syntax and interpreted
/// against the declared parameter's kind during resolution.
pub(crate) fn parse_generic_arg(p: &mut Parser) -> Option<GenericArg> {
    match parse_comp_literal(p) {
        Some(literal) => Some(GenericArg::Value(literal)),
        None => Some(GenericArg::Type(parse_type(p)?)),
    }
}

fn parse_function_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // '('
    let self_mode = parse_function_type_receiver(p);
    let mut params = Vec::new();
    let mut is_variadic = false;
    let mut needs_separator = self_mode.is_some();
    while !p.check(&TokenKind::RParen) {
        if needs_separator && !p.eat(&TokenKind::Comma) {
            break;
        }
        if !params.is_empty() && p.eat(&TokenKind::DotDotDot) {
            is_variadic = true;
            break;
        }
        params.push(parse_function_type_param(p)?);
        needs_separator = true;
    }
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type = parse_type(p)?;
    Some(Type::Function(FunctionType {
        params,
        return_type: Box::new(return_type),
        is_variadic,
        self_mode,
        convention: None,
    }))
}

/// A receiver is recognized only in its exact spellings, so a leading `*`
/// still begins an ordinary pointer-typed parameter such as `*Thing`.
fn parse_function_type_receiver(p: &mut Parser) -> Option<SelfMode> {
    use contextual::{MUT, SELF};

    let by_pointer = p.check(&TokenKind::Star);
    let head = usize::from(by_pointer);
    let mutable = p.at_contextual_at(head, MUT) && p.at_contextual_at(head + 1, SELF);
    if !mutable && !p.at_contextual_at(head, SELF) {
        return None;
    }
    for _ in 0..head + usize::from(mutable) + 1 {
        p.advance();
    }
    Some(match (by_pointer, mutable) {
        (false, false) => SelfMode::Value,
        (false, true) => SelfMode::MutValue,
        (true, false) => SelfMode::Pointer,
        (true, true) => SelfMode::MutPointer,
    })
}

/// `name : type` is the described form; anything else is a bare type. The
/// name is metadata, so only `identifier ":"` -- never `identifier "::"` --
/// distinguishes it from a parameter whose type is a plain path.
fn parse_function_type_param(p: &mut Parser) -> Option<FunctionTypeParam> {
    let start = p.peek_span();
    let name = if matches!(p.peek(), TokenKind::Ident(_)) && p.peek_at(1) == &TokenKind::Colon {
        let ident = p.expect_ident()?;
        p.advance(); // ':'
        Some(ident)
    } else {
        None
    };
    let r#type = parse_type(p)?;
    Some(FunctionTypeParam {
        name,
        span: start.to(p.last_span()),
        r#type,
    })
}

/// `foreign(cc) (...) => T` -- the parenthesized convention is mandatory and
/// immediately follows the keyword, so this can never be confused with the
/// function type's own parameter list. A bare `foreign (...) => T` therefore
/// fails here (expects an identifier where the params would start), which is
/// intentional: the ordinary type already denotes the Omega convention.
fn parse_foreign_function_type(p: &mut Parser) -> Option<Type> {
    p.advance(); // 'foreign'
    let convention = parse_raw_convention(p)?;
    if !p.check(&TokenKind::LParen) {
        p.error(ParseErrorKind::Expected {
            expected: "a function type '(...) => T' after 'foreign(cc)'",
            found: p.peek().describe(),
        });
        return None;
    }
    let Some(Type::Function(mut function_type)) = parse_function_type(p) else {
        return None;
    };
    function_type.convention = Some(convention);
    Some(Type::Function(function_type))
}

pub(crate) fn parse_raw_convention(p: &mut Parser) -> Option<RawConvention> {
    p.expect(&TokenKind::LParen, "'('");
    let name = p.expect_ident()?;
    let span = p.last_span();
    p.expect(&TokenKind::RParen, "')'");
    Some(RawConvention { name, span })
}

fn parse_named_type(p: &mut Parser) -> Option<Type> {
    let path = parse_path(p)?;
    if p.eat(&TokenKind::Lt) {
        let mut args = vec![parse_generic_arg(p)?];
        while p.eat(&TokenKind::Comma) {
            args.push(parse_generic_arg(p)?);
        }
        p.expect_close_angle("'>'");
        Some(Type::Generic(path, args))
    } else {
        Some(Type::Named(path))
    }
}
