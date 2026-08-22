use crate::ast::expression::NumberBase;
use crate::ast::r#type::{FunctionType, RawConvention, Type};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, contextual, parse_path};

pub fn parse_type(p: &mut Parser) -> Option<Type> {
    p.descend(|p| match p.peek() {
        TokenKind::Star => parse_pointer_type(p),
        TokenKind::LBracket => parse_bracket_type(p),
        TokenKind::LParen => parse_function_type(p),
        TokenKind::Spec => parse_spec_static_type(p),
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
        convention: None,
    }))
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
