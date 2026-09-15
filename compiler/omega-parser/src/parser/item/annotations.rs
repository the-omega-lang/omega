use crate::ast::annotation::{
    AnnotationArg, AnnotationExpr, AnnotationExprKind, AnnotationLiteral, AnnotationNode,
};
use crate::diagnostics::{ParseError, ParseErrorKind, Span};
use crate::lexer::{TokenKind, tokenize};
use crate::parser::{Parser, contextual, recovery};

/// The annotation that decides whether a top-level item exists at all. It is
/// consumed before any consumer sees the item, so it never reaches HIR.
pub const CONDITION: &str = "cond";

/// A top-level item's annotations, with its conditions kept apart: they
/// belong to the item's existence rather than to the declaration underneath,
/// and every other annotation still flows through the item parsers unchanged.
pub(super) struct ItemAnnotations {
    pub conditions: Vec<AnnotationNode>,
    pub annotations: Vec<AnnotationNode>,
}

impl ItemAnnotations {
    pub fn first_span(&self) -> Option<Span> {
        self.conditions
            .first()
            .or(self.annotations.first())
            .map(|annotation| annotation.span)
    }
}

pub(super) fn parse_item_annotations(p: &mut Parser) -> ItemAnnotations {
    let (conditions, annotations) = parse_annotation_list(p)
        .into_iter()
        .partition(|annotation| annotation.name.as_ref() == CONDITION);
    ItemAnnotations {
        conditions,
        annotations,
    }
}

/// Annotations on something that is not a top-level item. A condition selects
/// whole declarations, so one written here is rejected by the grammar rather
/// than reaching a consumer that has no way to honor it.
pub(super) fn parse_annotations(p: &mut Parser) -> Vec<AnnotationNode> {
    let mut annotations = parse_annotation_list(p);
    annotations.retain(|annotation| {
        let condition = annotation.name.as_ref() == CONDITION;
        if condition {
            p.error_at(annotation.span, ParseErrorKind::ConditionNotAllowedHere);
        }
        !condition
    });
    annotations
}

fn parse_annotation_list(p: &mut Parser) -> Vec<AnnotationNode> {
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
    let origin = p.peek_origin();
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
    Some(AnnotationNode {
        name,
        args,
        span,
        origin,
    })
}

fn parse_annotation_arg(p: &mut Parser) -> Option<AnnotationArg> {
    if matches!(p.peek(), TokenKind::Ident(_)) && matches!(p.peek_at(1), TokenKind::Eq) {
        let key = p.expect_ident()?;
        p.expect(&TokenKind::Eq, "'='");
        return Some(AnnotationArg::KeyValue(key, parse_annotation_expr(p)?));
    }
    Some(AnnotationArg::Positional(parse_annotation_expr(p)?))
}

fn parse_annotation_expr(p: &mut Parser) -> Option<AnnotationExpr> {
    p.descend(|p| {
        let start = p.peek_span();
        let origin = p.peek_origin();
        let kind = parse_annotation_expr_kind(p)?;
        Some(AnnotationExpr {
            kind,
            span: start.to(p.last_span()),
            origin,
        })
    })
}

fn parse_annotation_expr_kind(p: &mut Parser) -> Option<AnnotationExprKind> {
    if let Some(literal) = parse_literal_token(p) {
        return Some(AnnotationExprKind::Literal(literal));
    }
    match p.peek() {
        TokenKind::Amp => {
            p.advance();
            p.expect(&TokenKind::LBracket, "'['");
            let elements = parse_expr_list(p, &TokenKind::RBracket, "']'")?;
            Some(AnnotationExprKind::List(elements))
        }
        TokenKind::Ident(name)
            if name == contextual::SIZEOF && matches!(p.peek_at(1), TokenKind::Lt) =>
        {
            p.advance(); // 'sizeof'
            p.expect(&TokenKind::Lt, "'<'");
            let r#type = crate::parser::r#type::parse_type(p)?;
            p.expect_close_angle("'>'");
            Some(AnnotationExprKind::Sizeof(r#type))
        }
        TokenKind::Ident(_) => {
            let name = p.expect_ident()?;
            if p.eat(&TokenKind::ColonColon) {
                return Some(AnnotationExprKind::Qualified {
                    namespace: name,
                    name: p.expect_ident()?,
                });
            }
            if p.eat(&TokenKind::LParen) {
                let args = parse_expr_list(p, &TokenKind::RParen, "')'")?;
                return Some(AnnotationExprKind::Call { name, args });
            }
            Some(AnnotationExprKind::Name(name))
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: ANNOTATION_ARGUMENT,
                found: p.peek().describe(),
            });
            None
        }
    }
}

const ANNOTATION_ARGUMENT: &str =
    "an annotation argument: a literal, a name, 'name(...)', '&[...]', or 'sizeof<Type>'";

/// Nested argument lists accept a trailing comma so a condition can be spread
/// over several lines and edited a line at a time. A missing separator is
/// still an error: the closing delimiter is what ends the list.
fn parse_expr_list(
    p: &mut Parser,
    close: &TokenKind,
    expected: &'static str,
) -> Option<Vec<AnnotationExpr>> {
    let mut elements = Vec::new();
    while !p.check(close) {
        elements.push(parse_annotation_expr(p)?);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
    }
    p.expect(close, expected).then_some(elements)
}

fn parse_literal_token(p: &mut Parser) -> Option<AnnotationLiteral> {
    let literal = match p.peek() {
        TokenKind::True => AnnotationLiteral::Bool(true),
        TokenKind::False => AnnotationLiteral::Bool(false),
        TokenKind::Char(c) => AnnotationLiteral::Char(*c),
        TokenKind::Str(s) => AnnotationLiteral::Str(s.clone()),
        TokenKind::ByteStr(s) => AnnotationLiteral::ByteStr(s.clone()),
        TokenKind::Number(n) => AnnotationLiteral::Number {
            negative: false,
            value: n.clone(),
        },
        TokenKind::Minus => {
            let TokenKind::Number(n) = p.peek_at(1) else {
                return None;
            };
            let value = n.clone();
            p.advance(); // '-'
            p.advance();
            return Some(AnnotationLiteral::Number {
                negative: true,
                value,
            });
        }
        _ => return None,
    };
    p.advance();
    Some(literal)
}

pub(super) fn reject_annotations(p: &mut Parser, annotations: &[AnnotationNode]) {
    if let Some(first) = annotations.first() {
        p.error_at(first.span, ParseErrorKind::AnnotationNotAllowedHere);
    }
}

/// Decodes one complete Omega literal written outside any source file, such
/// as a compiler definition supplied on the command line. The whole input
/// must be exactly one literal: it is a value, not an expression.
pub fn parse_literal(source: &str) -> Result<AnnotationLiteral, LiteralError> {
    let (tokens, errors) = tokenize(source);
    if let Some(error) = errors.into_iter().next() {
        return Err(LiteralError::Malformed(error));
    }
    let mut p = Parser::new(&tokens);
    let Some(literal) = parse_literal_token(&mut p) else {
        return Err(LiteralError::NotALiteral);
    };
    if !p.is_eof() {
        return Err(LiteralError::TrailingInput);
    }
    Ok(literal)
}

#[derive(Debug)]
pub enum LiteralError {
    Malformed(ParseError),
    NotALiteral,
    TrailingInput,
}

impl std::fmt::Display for LiteralError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(error) => write!(f, "{error}"),
            Self::NotALiteral => f.write_str(
                "expected one literal value: a boolean, number, character, string, or byte string",
            ),
            Self::TrailingInput => {
                f.write_str("expected exactly one literal value, with nothing after it")
            }
        }
    }
}
