use crate::ast::annotation::{AnnotationArg, AnnotationNode, AnnotationValue};
use crate::ast::expression::NumberBase;
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, contextual, recovery};

pub(super) fn parse_annotations(p: &mut Parser) -> Vec<AnnotationNode> {
    let mut annotations = Vec::new();
    while p.check(&TokenKind::At) {
        match parse_annotation(p) {
            Some(annotation) => annotations.push(annotation),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    annotations
}

fn parse_annotation(p: &mut Parser) -> Option<AnnotationNode> {
    let start = p.peek_span();
    p.expect(&TokenKind::At, "'@'");
    let name = p.expect_ident()?;
    let mut args = Vec::new();
    if p.eat(&TokenKind::LParen) {
        if !p.check(&TokenKind::RParen) {
            loop {
                args.push(parse_annotation_arg(p)?);
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        p.expect(&TokenKind::RParen, "')'");
    }
    let span = start.to(p.last_span());
    Some(AnnotationNode { name, args, span })
}

fn parse_annotation_arg(p: &mut Parser) -> Option<AnnotationArg> {
    let ident = p.expect_ident()?;
    if !p.eat(&TokenKind::Eq) {
        return Some(AnnotationArg::Ident(ident));
    }
    match p.peek() {
        TokenKind::Number(n)
            if matches!(n.base, NumberBase::Decimal)
                && n.fractional_part.is_none()
                && n.explicit_type.is_none() =>
        {
            let value = n.integer_part.clone();
            p.advance();
            Some(AnnotationArg::KeyValue(
                ident,
                AnnotationValue::IntLiteral(value),
            ))
        }
        TokenKind::Ident(name)
            if name == contextual::SIZEOF && matches!(p.peek_at(1), TokenKind::Lt) =>
        {
            p.advance(); // 'sizeof'
            p.expect(&TokenKind::Lt, "'<'");
            let r#type = crate::parser::r#type::parse_type(p)?;
            p.expect_close_angle("'>'");
            Some(AnnotationArg::KeyValue(
                ident,
                AnnotationValue::Sizeof(r#type),
            ))
        }
        TokenKind::Str(_) => {
            let TokenKind::Str(s) = p.advance().kind else {
                unreachable!()
            };
            Some(AnnotationArg::KeyValue(
                ident,
                AnnotationValue::StrLiteral(s),
            ))
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "a plain integer, 'sizeof<Type>', or a string literal",
                found: p.peek().describe(),
            });
            None
        }
    }
}

pub(super) fn reject_annotations(p: &mut Parser, annotations: &[AnnotationNode]) {
    if let Some(first) = annotations.first() {
        p.error_at(first.span, ParseErrorKind::AnnotationNotAllowedHere);
    }
}
