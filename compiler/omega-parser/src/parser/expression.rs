use crate::ast::expression::{
    AddressOfExpr, ArrayLiteralExpr, AssignmentExpr, BinaryOp, BinaryOpExpr, BitNotExpr, BoolExpr,
    ByteStringExpr, CastExpr, CharExpr, CodeblockExpr, CompExpr, CompoundAssignExpr, DecrementExpr,
    DerefExpr, Expression, ExpressionNode, FieldAccessExpr, FunctionCallExpr, IfExpr,
    IncrementExpr, IndexExpr, LogicalExpr, LogicalOp, MatchArm, MatchExpr, NegateExpr, NotExpr,
    Pattern, PatternValue, RevealExpr, SizeofExpr, SliceExpr, StringExpr, StructLiteralExpr,
    StructLiteralField,
};
use crate::ast::range::{RangeEnd, RangeExpr};
use crate::ast::r#type::Type;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::{
    Mark, Parser, contextual, macro_syntax::parse_macro_invocation, statement::parse_statement,
};

pub fn parse_expression(p: &mut Parser) -> Option<ExpressionNode> {
    p.descend(parse_range_or_expression)
}

fn parse_assignment(p: &mut Parser) -> Option<ExpressionNode> {
    let target = parse_logical_or(p)?;
    let op = match p.peek() {
        TokenKind::Eq => None,
        TokenKind::PlusEq => Some(BinaryOp::Add),
        TokenKind::MinusEq => Some(BinaryOp::Sub),
        TokenKind::StarEq => Some(BinaryOp::Mul),
        TokenKind::SlashEq => Some(BinaryOp::Div),
        TokenKind::PercentEq => Some(BinaryOp::Rem),
        TokenKind::AmpEq => Some(BinaryOp::BitAnd),
        TokenKind::PipeEq => Some(BinaryOp::BitOr),
        TokenKind::CaretEq => Some(BinaryOp::BitXor),
        TokenKind::ShlEq => Some(BinaryOp::Shl),
        TokenKind::ShrEq => Some(BinaryOp::Shr),
        _ => return Some(target),
    };
    p.advance();
    let value = parse_expression(p)?;
    let span = target.span.to(value.span);
    let expression = match op {
        None => Expression::Assignment(Box::new(AssignmentExpr {
            target,
            value: Box::new(value),
        })),
        Some(op) => Expression::CompoundAssign(Box::new(CompoundAssignExpr {
            target,
            op,
            value: Box::new(value),
        })),
    };
    Some(ExpressionNode { expression, span })
}

const BINARY_TIERS: &[&[(TokenKind, BinaryOp)]] = &[
    &[(TokenKind::Pipe, BinaryOp::BitOr)],
    &[(TokenKind::Caret, BinaryOp::BitXor)],
    &[(TokenKind::Amp, BinaryOp::BitAnd)],
    &[
        (TokenKind::Shl, BinaryOp::Shl),
        (TokenKind::Shr, BinaryOp::Shr),
    ],
    &[
        (TokenKind::Plus, BinaryOp::Add),
        (TokenKind::Minus, BinaryOp::Sub),
    ],
    &[
        (TokenKind::Star, BinaryOp::Mul),
        (TokenKind::Slash, BinaryOp::Div),
        (TokenKind::Percent, BinaryOp::Rem),
    ],
];

fn parse_logical_or(p: &mut Parser) -> Option<ExpressionNode> {
    let mut left = parse_logical_and(p)?;
    while p.check(&TokenKind::PipePipe) {
        p.advance();
        let right = parse_logical_and(p)?;
        let span = left.span.to(right.span);
        left = ExpressionNode {
            expression: Expression::Logical(Box::new(LogicalExpr {
                op: LogicalOp::Or,
                left,
                right,
            })),
            span,
        };
    }
    Some(left)
}

fn parse_logical_and(p: &mut Parser) -> Option<ExpressionNode> {
    let mut left = parse_comparison(p)?;
    while p.check(&TokenKind::AmpAmp) {
        p.advance();
        let right = parse_comparison(p)?;
        let span = left.span.to(right.span);
        left = ExpressionNode {
            expression: Expression::Logical(Box::new(LogicalExpr {
                op: LogicalOp::And,
                left,
                right,
            })),
            span,
        };
    }
    Some(left)
}

fn parse_comparison(p: &mut Parser) -> Option<ExpressionNode> {
    let left = parse_binary_tier(p, 0)?;
    let op = match p.peek() {
        TokenKind::EqEq => BinaryOp::Eq,
        TokenKind::NotEq => BinaryOp::Ne,
        TokenKind::LtEq => BinaryOp::Le,
        TokenKind::GtEq => BinaryOp::Ge,
        TokenKind::Lt => BinaryOp::Lt,
        TokenKind::Gt => BinaryOp::Gt,
        _ => return Some(left),
    };
    p.advance();
    let right = parse_binary_tier(p, 0)?;
    if matches!(
        p.peek(),
        TokenKind::EqEq
            | TokenKind::NotEq
            | TokenKind::LtEq
            | TokenKind::GtEq
            | TokenKind::Lt
            | TokenKind::Gt
    ) {
        p.error(ParseErrorKind::ChainedComparison);
        p.advance();
        parse_binary_tier(p, 0)?;
    }
    let span = left.span.to(right.span);
    Some(ExpressionNode {
        expression: binary_op_expr(left, op, right),
        span,
    })
}

fn parse_binary_tier(p: &mut Parser, tier: usize) -> Option<ExpressionNode> {
    let mut left = if tier == BINARY_TIERS.len() {
        parse_unary(p)?
    } else {
        parse_binary_tier(p, tier + 1)?
    };
    if tier == BINARY_TIERS.len() {
        return Some(left);
    }
    while let Some((_, op)) = BINARY_TIERS[tier].iter().find(|(kind, _)| p.check(kind)) {
        p.advance();
        let right = parse_binary_tier(p, tier + 1)?;
        let span = left.span.to(right.span);
        left = ExpressionNode {
            expression: binary_op_expr(left, *op, right),
            span,
        };
    }
    Some(left)
}

fn binary_op_expr(left: ExpressionNode, op: BinaryOp, right: ExpressionNode) -> Expression {
    Expression::BinaryOp(Box::new(BinaryOpExpr { left, op, right }))
}

fn parse_unary(p: &mut Parser) -> Option<ExpressionNode> {
    use crate::parser::contextual::{COMP, MUT, REVEAL};
    let start = p.peek_span();
    // A leading `<` uniquely starts cast syntax here; comparison `<` is infix.
    if p.check(&TokenKind::Lt) {
        return parse_cast(p, start);
    }
    enum Prefix {
        Deref,
        AddressOf { mutable: bool },
        Negate,
        BitNot,
        Not,
        Increment,
        Decrement,
        Reveal,
        Comp,
    }
    let prefix = match p.peek() {
        TokenKind::PlusPlus => Prefix::Increment,
        TokenKind::MinusMinus => Prefix::Decrement,
        TokenKind::Star => Prefix::Deref,
        TokenKind::Amp if p.at_contextual_at(1, MUT) => Prefix::AddressOf { mutable: true },
        TokenKind::Amp => Prefix::AddressOf { mutable: false },
        TokenKind::Minus => Prefix::Negate,
        TokenKind::Tilde => Prefix::BitNot,
        TokenKind::Not => Prefix::Not,
        TokenKind::Ident(_) if p.at_contextual(REVEAL) && operand_follows(p) => Prefix::Reveal,
        TokenKind::Ident(_) if p.at_contextual(COMP) && operand_follows(p) => Prefix::Comp,
        _ => return parse_postfix(p),
    };
    p.advance();
    if matches!(prefix, Prefix::AddressOf { mutable: true }) {
        p.advance(); // 'mut'
    }
    let base = parse_unary(p)?;
    let span = start.to(base.span);
    let expression = match prefix {
        Prefix::Deref => Expression::Deref(Box::new(DerefExpr { base })),
        Prefix::AddressOf { mutable } => {
            Expression::AddressOf(Box::new(AddressOfExpr { base, mutable }))
        }
        Prefix::Negate => Expression::Negate(Box::new(NegateExpr { base })),
        Prefix::BitNot => Expression::BitNot(Box::new(BitNotExpr { base })),
        Prefix::Not => Expression::Not(Box::new(NotExpr { base })),
        Prefix::Reveal => Expression::Reveal(Box::new(RevealExpr { base })),
        Prefix::Comp => Expression::Comp(Box::new(CompExpr { base })),
        Prefix::Increment => Expression::Increment(Box::new(IncrementExpr { base })),
        Prefix::Decrement => Expression::Decrement(Box::new(DecrementExpr { base })),
    };
    Some(ExpressionNode { expression, span })
}

fn parse_cast(p: &mut Parser, start: Span) -> Option<ExpressionNode> {
    p.advance(); // '<'
    let target = crate::parser::r#type::parse_type(p)?;
    if p.eat(&TokenKind::Colon) {
        let spec = crate::parser::r#type::parse_type(p)?;
        p.expect_close_angle("'>'");
        p.expect(&TokenKind::ColonColon, "'::'");
        let (function, origin) = p.expect_ident_with_origin()?;
        let span = start.to(p.last_span());
        let path = ExpressionNode {
            expression: Expression::Path(crate::ast::identifier::ExprPath {
                path: crate::ast::identifier::Path {
                    anchor: None,
                    head: function,
                    tail: Vec::new(),
                    origin,
                },
                generic_args: Vec::new(),
                args_at: 0,
                qualified_spec: Some(crate::ast::identifier::QualifiedSpecPath { target, spec }),
            }),
            span,
        };
        // The `(...)` call (and any further postfix) attaches to the path,
        // exactly like an ordinary callee's would -- `<S : P>::make()` is a
        // call, so the arguments are part of this same expression.
        return parse_postfix_loop(p, path);
    }
    p.expect_close_angle("'>'");
    let base = parse_unary(p)?;
    let span = start.to(base.span);
    Some(ExpressionNode {
        expression: Expression::Cast(Box::new(CastExpr { target, base })),
        span,
    })
}

fn parse_postfix(p: &mut Parser) -> Option<ExpressionNode> {
    let expr = parse_primary(p)?;
    parse_postfix_loop(p, expr)
}

fn parse_postfix_loop(p: &mut Parser, mut expr: ExpressionNode) -> Option<ExpressionNode> {
    loop {
        match p.peek() {
            TokenKind::Dot => {
                p.advance();
                let field_span = p.peek_span();
                let field = p.expect_ident()?;
                let span = expr.span.to(field_span);
                expr = ExpressionNode {
                    expression: Expression::FieldAccess(Box::new(FieldAccessExpr {
                        base: expr,
                        field,
                    })),
                    span,
                };
            }
            TokenKind::LBracket => {
                expr = parse_index_or_slice(p, expr)?;
            }
            TokenKind::LParen => {
                expr = parse_call(p, expr)?;
            }
            _ => break,
        }
    }
    Some(expr)
}

fn parse_index_or_slice(p: &mut Parser, base: ExpressionNode) -> Option<ExpressionNode> {
    p.advance(); // '['
    if is_range_operator(p.peek()) {
        let op_span = p.peek_span();
        let range = p.allow_struct_literals(|p| parse_range_tail(p, None, op_span))?;
        return finish_slice(p, base, range);
    }
    // Parse the potential first bound below the range layer: otherwise
    // `0..<end` would be consumed as a standalone range before this
    // production has a chance to recognize it as a slice.
    let first = p.allow_struct_literals(parse_assignment)?;
    if is_range_operator(p.peek()) {
        let op_span = p.peek_span();
        let range = p.allow_struct_literals(|p| parse_range_tail(p, Some(first), op_span))?;
        return finish_slice(p, base, range);
    }
    let close_span = p.peek_span();
    p.expect(&TokenKind::RBracket, "']'");
    let span = base.span.to(close_span);
    Some(ExpressionNode {
        expression: Expression::Index(Box::new(IndexExpr { base, index: first })),
        span,
    })
}

fn is_range_operator(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::DotDotEq | TokenKind::DotDotLt | TokenKind::DotDot
    )
}

fn parse_range_tail(
    p: &mut Parser,
    start: Option<ExpressionNode>,
    op_span: Span,
) -> Option<RangeExpr> {
    let op = p.peek().clone();
    p.advance();
    let end = match op {
        // Bare `..` means an omitted end; an explicit end must use `..<` or `..=`.
        TokenKind::DotDot => {
            if expression_starts_here(p) {
                p.error_at(op_span, ParseErrorKind::OpenRangeHasEnd);
                return None;
            }
            RangeEnd::Open
        }
        // `..=`/`..<` require an explicit end.
        TokenKind::DotDotLt | TokenKind::DotDotEq => {
            if !expression_starts_here(p) {
                p.error_at(op_span, ParseErrorKind::RangeMissingEnd);
                return None;
            }
            let end = parse_assignment(p)?;
            if op == TokenKind::DotDotEq {
                RangeEnd::Inclusive(end)
            } else {
                RangeEnd::Exclusive(end)
            }
        }
        _ => unreachable!("caller already confirmed a range operator is here"),
    };
    let lo = start.as_ref().map(|s| s.span).unwrap_or(op_span);
    let hi = end.end_expr().map(|e| e.span).unwrap_or(op_span);
    Some(RangeExpr {
        start,
        end,
        span: lo.to(hi),
    })
}

pub(crate) fn parse_range_or_expression(p: &mut Parser) -> Option<ExpressionNode> {
    if is_range_operator(p.peek()) {
        let op_span = p.peek_span();
        let range = parse_range_tail(p, None, op_span)?;
        let span = range.span;
        return Some(ExpressionNode {
            expression: Expression::Range(Box::new(range)),
            span,
        });
    }
    let first = parse_assignment(p)?;
    if is_range_operator(p.peek()) {
        let op_span = p.peek_span();
        let start_span = first.span;
        let range = parse_range_tail(p, Some(first), op_span)?;
        let span = start_span.to(range.span);
        return Some(ExpressionNode {
            expression: Expression::Range(Box::new(range)),
            span,
        });
    }
    Some(first)
}

fn expression_starts_here(p: &Parser) -> bool {
    expression_starts_at(p, 0)
}

fn operand_follows(p: &Parser) -> bool {
    expression_starts_at(p, 1) || matches!(p.peek_at(1), TokenKind::Lt)
}

fn expression_starts_at(p: &Parser, offset: usize) -> bool {
    if matches!(p.peek_at(offset), TokenKind::LBrace) {
        return p.struct_literals_allowed();
    }
    matches!(
        p.peek_at(offset),
        TokenKind::Ident(_)
            | TokenKind::Number(_)
            | TokenKind::Str(_)
            | TokenKind::ByteStr(_)
            | TokenKind::Char(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::If
            | TokenKind::Match
            | TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::Amp
            | TokenKind::Star
            | TokenKind::Minus
            | TokenKind::Tilde
            | TokenKind::Not
            | TokenKind::PlusPlus
            | TokenKind::MinusMinus
    )
}

fn finish_slice(p: &mut Parser, base: ExpressionNode, range: RangeExpr) -> Option<ExpressionNode> {
    let close_span = p.peek_span();
    p.expect(&TokenKind::RBracket, "']'");
    let span = base.span.to(close_span);
    Some(ExpressionNode {
        expression: Expression::Slice(Box::new(SliceExpr { base, range })),
        span,
    })
}

fn parse_call(p: &mut Parser, callee: ExpressionNode) -> Option<ExpressionNode> {
    p.advance(); // '('
    let mut args = Vec::new();
    if !p.check(&TokenKind::RParen) {
        loop {
            args.push(p.allow_struct_literals(parse_expression)?);
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
    }
    let close_span = p.peek_span();
    p.expect(&TokenKind::RParen, "')'");
    let span = callee.span.to(close_span);
    Some(ExpressionNode {
        expression: Expression::FunctionCall(FunctionCallExpr {
            callee: Box::new(callee),
            args,
        }),
        span,
    })
}

fn parse_block_shaped_primary(p: &mut Parser, start: Span) -> Option<ExpressionNode> {
    match p.peek() {
        TokenKind::LBrace => {
            let cb = parse_codeblock(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode {
                expression: Expression::Codeblock(cb),
                span,
            })
        }
        TokenKind::If => {
            let if_expr = parse_if_expr(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode {
                expression: Expression::If(Box::new(if_expr)),
                span,
            })
        }
        TokenKind::Match => {
            let match_expr = parse_match_expr(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode {
                expression: Expression::Match(Box::new(match_expr)),
                span,
            })
        }
        _ => unreachable!("parse_block_shaped_primary called on a non-block-shaped token"),
    }
}

pub(crate) fn parse_statement_leading_expression(p: &mut Parser) -> Option<ExpressionNode> {
    let start = p.peek_span();
    if matches!(
        p.peek(),
        TokenKind::LBrace | TokenKind::If | TokenKind::Match
    ) {
        return parse_block_shaped_primary(p, start);
    }
    parse_expression(p)
}

fn parse_primary(p: &mut Parser) -> Option<ExpressionNode> {
    let start = p.peek_span();
    match p.peek() {
        TokenKind::LParen => {
            p.advance();
            let inner = p.allow_struct_literals(parse_expression)?;
            p.expect(&TokenKind::RParen, "')'");
            // Deliberately keeps `inner`'s own span, not one extended to
            // cover the parens -- matches the old grammar, which never
            // re-wrapped a parenthesized expression's span either.
            Some(inner)
        }
        TokenKind::LBrace | TokenKind::If | TokenKind::Match => {
            parse_block_shaped_primary(p, start)
        }
        TokenKind::LBracket => parse_array_literal(p),
        TokenKind::Number(_) => {
            let TokenKind::Number(n) = p.advance().kind else {
                unreachable!()
            };
            Some(ExpressionNode {
                expression: Expression::Number(n),
                span: start,
            })
        }
        TokenKind::Str(_) => {
            let TokenKind::Str(s) = p.advance().kind else {
                unreachable!()
            };
            Some(ExpressionNode {
                expression: Expression::String(StringExpr(s)),
                span: start,
            })
        }
        TokenKind::ByteStr(_) => {
            let TokenKind::ByteStr(s) = p.advance().kind else {
                unreachable!()
            };
            Some(ExpressionNode {
                expression: Expression::ByteString(ByteStringExpr(s)),
                span: start,
            })
        }
        TokenKind::Char(_) => {
            let TokenKind::Char(c) = p.advance().kind else {
                unreachable!()
            };
            Some(ExpressionNode {
                expression: Expression::Char(CharExpr(c)),
                span: start,
            })
        }
        TokenKind::True => {
            p.advance();
            Some(ExpressionNode {
                expression: Expression::Bool(BoolExpr(true)),
                span: start,
            })
        }
        TokenKind::False => {
            p.advance();
            Some(ExpressionNode {
                expression: Expression::Bool(BoolExpr(false)),
                span: start,
            })
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Dollar) => {
            let inv = parse_macro_invocation(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode {
                expression: Expression::MacroInvocation(inv),
                span,
            })
        }
        // Commit contextual `sizeof` only when `<Type>` follows.
        TokenKind::Ident(name)
            if name == contextual::SIZEOF && matches!(p.peek_at(1), TokenKind::Lt) =>
        {
            p.advance(); // 'sizeof'
            p.advance(); // '<'
            let r#type = crate::parser::r#type::parse_type(p)?;
            let close_span = p.peek_span();
            p.expect_close_angle("'>'");
            let span = start.to(close_span);
            Some(ExpressionNode {
                expression: Expression::Sizeof(Box::new(SizeofExpr { r#type })),
                span,
            })
        }
        TokenKind::Ident(_) => {
            let path = parse_expr_path(p)?;
            if p.check(&TokenKind::LBrace) {
                if p.struct_literals_allowed() {
                    return parse_struct_literal(p, path, start);
                }
                if let Some(literal) = recover_restricted_struct_literal(p, &path, start) {
                    return Some(literal);
                }
            }
            let span = start.to(p.last_span());
            Some(ExpressionNode {
                expression: Expression::Path(path),
                span,
            })
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "an expression",
                found: p.peek().describe(),
            });
            None
        }
    }
}

fn parse_expr_path(p: &mut Parser) -> Option<crate::ast::identifier::ExprPath> {
    use crate::ast::identifier::ExprPath;

    let anchor = crate::parser::parse_path_anchor(p);
    let (head, origin) = p.expect_ident_with_origin()?;
    let mut path = crate::ast::identifier::Path {
        anchor,
        head,
        tail: Vec::new(),
        origin,
    };
    let mut generic_args = Vec::new();
    let mut args_at = 0;

    loop {
        let segment = path.tail.len();
        if generic_args.is_empty()
            && p.check(&TokenKind::Lt)
            && let Some(args) = try_parse_generic_args(p)
        {
            generic_args = args;
            args_at = segment;
        }
        if !p.check(&TokenKind::ColonColon) {
            break;
        }
        p.advance();
        path.tail.push(p.expect_ident()?);
    }

    Some(ExprPath {
        path,
        generic_args,
        args_at,
        qualified_spec: None,
    })
}

fn try_parse_generic_args(p: &mut Parser) -> Option<Vec<crate::ast::r#type::Type>> {
    let mark = p.mark();
    p.advance(); // '<'
    let mut args = Vec::new();
    loop {
        match crate::parser::r#type::parse_type(p) {
            Some(ty) => args.push(ty),
            None => {
                p.reset(mark);
                return None;
            }
        }
        if !p.eat(&TokenKind::Comma) {
            break;
        }
    }
    if !p.eat_close_angle() {
        p.reset(mark);
        return None;
    }
    let commits = p.check(&TokenKind::ColonColon)
        || (p.check(&TokenKind::LBrace) && p.struct_literals_allowed());
    if !commits {
        p.reset(mark);
        return None;
    }
    Some(args)
}

fn recover_restricted_struct_literal(
    p: &mut Parser,
    path: &crate::ast::identifier::ExprPath,
    start: crate::diagnostics::Span,
) -> Option<ExpressionNode> {
    let mark = p.mark();
    let Some(literal) = parse_struct_literal(p, path.clone(), start) else {
        p.reset(mark);
        return None;
    };
    let confirms_literal = matches!(
        p.peek(),
        TokenKind::LBrace
            | TokenKind::Dot
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqEq
            | TokenKind::NotEq
            | TokenKind::Lt
            | TokenKind::LtEq
            | TokenKind::Gt
            | TokenKind::GtEq
    );
    if !confirms_literal {
        p.reset(mark);
        return None;
    }
    p.error_at(literal.span, ParseErrorKind::StructLiteralNotAllowedHere);
    Some(literal)
}

fn parse_struct_literal(
    p: &mut Parser,
    path: crate::ast::identifier::ExprPath,
    start: crate::diagnostics::Span,
) -> Option<ExpressionNode> {
    p.expect(&TokenKind::LBrace, "'{'");
    let mut fields = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        let name_span = p.peek_span();
        let name = p.expect_ident()?;
        p.expect(&TokenKind::Eq, "'='");
        // Inside the literal's braces, a nested struct literal is
        // unambiguous again even if this one sits in condition position.
        let value = p.allow_struct_literals(parse_expression)?;
        p.expect_terminator(&TokenKind::Semi, "';'");
        fields.push(StructLiteralField {
            name,
            name_span,
            value,
        });
    }
    if !p.check(&TokenKind::RBrace) {
        p.error(ParseErrorKind::Expected {
            expected: "a field initializer (`name = value;`) or '}'",
            found: p.peek().describe(),
        });
        return None;
    }
    p.advance(); // '}'
    let span = start.to(p.last_span());
    Some(ExpressionNode {
        expression: Expression::StructLiteral(StructLiteralExpr { path, fields }),
        span,
    })
}

fn parse_array_literal(p: &mut Parser) -> Option<ExpressionNode> {
    let start = p.peek_span();
    p.advance(); // '['
    let mut elements = Vec::new();
    if !p.check(&TokenKind::RBracket) {
        loop {
            elements.push(p.allow_struct_literals(parse_expression)?);
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
    }
    p.expect(&TokenKind::RBracket, "']'");
    let span = start.to(p.last_span());
    Some(ExpressionNode {
        expression: Expression::ArrayLiteral(ArrayLiteralExpr { elements }),
        span,
    })
}

fn parse_if_expr(p: &mut Parser) -> Option<IfExpr> {
    p.expect(&TokenKind::If, "'if'");
    let mut branches = vec![parse_if_branch_body(p)?];
    let mut else_branch = None;
    loop {
        if !p.check(&TokenKind::Else) {
            break;
        }
        p.advance();
        if p.eat(&TokenKind::If) {
            branches.push(parse_if_branch_body(p)?);
        } else {
            else_branch = Some(parse_codeblock(p)?);
            break;
        }
    }
    Some(IfExpr {
        branches,
        else_branch,
    })
}

fn parse_if_branch_body(p: &mut Parser) -> Option<(ExpressionNode, CodeblockExpr)> {
    let condition = p.restrict_struct_literals(parse_expression)?;
    let body = parse_codeblock(p)?;
    Some((condition, body))
}

fn parse_match_expr(p: &mut Parser) -> Option<MatchExpr> {
    let start = p.peek_span();
    p.expect(&TokenKind::Match, "'match'");
    let scrutinee = p.restrict_struct_literals(parse_expression)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let mut arms = Vec::new();
    while !p.check(&TokenKind::RBrace) && !p.is_eof() {
        arms.push(parse_match_arm(p)?);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
    }
    p.expect(&TokenKind::RBrace, "'}'");
    let else_branch = if p.eat(&TokenKind::Else) {
        Some(parse_codeblock(p)?)
    } else {
        None
    };
    let span = start.to(p.last_span());
    Some(MatchExpr {
        scrutinee,
        arms,
        else_branch,
        span,
    })
}

fn parse_match_arm(p: &mut Parser) -> Option<MatchArm> {
    let start = p.peek_span();
    let pattern = parse_pattern(p)?;
    p.expect(&TokenKind::FatArrow, "'=>'");
    let body = p.allow_struct_literals(parse_expression)?;
    let span = start.to(body.span);
    Some(MatchArm {
        pattern,
        body,
        span,
    })
}

fn parse_pattern(p: &mut Parser) -> Option<Pattern> {
    let start = p.peek_span();
    let candidate = probe_type_pattern(p);
    let value = parse_pattern_value(p, candidate.is_some());
    if let (None, Some(candidate)) = (&value, &candidate) {
        // Only a spelling with no value reading at all -- `[4]u8`,
        // `Wrapper<i32>` -- consumes the type reading's tokens instead.
        let end = candidate.end;
        p.reset(end);
    }
    let span = start.to(p.last_span());
    let r#type = candidate.map(|candidate| candidate.r#type);
    if value.is_none() && r#type.is_none() {
        return None;
    }
    Some(Pattern {
        value,
        r#type,
        span,
    })
}

struct TypePatternCandidate {
    r#type: Type,
    end: Mark,
}

/// A non-diagnostic probe for a pattern that could also be read as a type.
/// The candidate is kept only when a complete type parse consumed everything
/// up to the arm's `=>` and produced no diagnostics of its own, so an
/// ambiguous path pattern such as `A` keeps both readings and the analyzer
/// decides between them from the scrutinee's type.
fn probe_type_pattern(p: &mut Parser) -> Option<TypePatternCandidate> {
    let start = p.mark();
    let parsed = crate::parser::r#type::parse_type(p);
    let end = p.mark();
    let candidate = match parsed {
        Some(r#type) if p.check(&TokenKind::FatArrow) && !p.errors_since(&start) => {
            Some(TypePatternCandidate { r#type, end })
        }
        _ => None,
    };
    p.reset(start);
    candidate
}

/// `has_type_candidate` decides whether an unusable value reading may be
/// abandoned. Without a type reading to fall back on there is nothing to
/// abandon it *for*, so the parse and its diagnostics stay exactly what they
/// were before type patterns existed.
fn parse_pattern_value(p: &mut Parser, has_type_candidate: bool) -> Option<PatternValue> {
    if is_range_operator(p.peek()) {
        let op_span = p.peek_span();
        let range = p.allow_struct_literals(|p| parse_range_tail(p, None, op_span))?;
        return Some(PatternValue::Range(range));
    }
    // Keep range syntax structural in patterns; the general expression layer
    // would otherwise consume `a..<b` before this production sees it.
    let mark = p.mark();
    let Some(value) = p.allow_struct_literals(parse_assignment) else {
        if has_type_candidate {
            p.reset(mark);
        }
        return None;
    };
    if is_range_operator(p.peek()) {
        let op_span = p.peek_span();
        let range = p.allow_struct_literals(|p| parse_range_tail(p, Some(value), op_span))?;
        return Some(PatternValue::Range(range));
    }
    // A value reading that stalls before the arm's `=>` is not a pattern at
    // all: `[4]u8` parses `[4]` and then stops. Handing the arm to the type
    // reading avoids a spurious "expected '=>'".
    if has_type_candidate && !p.check(&TokenKind::FatArrow) {
        p.reset(mark);
        return None;
    }
    Some(PatternValue::Value(value))
}

pub fn parse_codeblock(p: &mut Parser) -> Option<CodeblockExpr> {
    // Inside the block's own braces, struct literals are unambiguous again
    // regardless of what position the block itself sits in.
    p.allow_struct_literals(|p| {
        let start = p.peek_span();
        p.expect(&TokenKind::LBrace, "'{'");
        let mut cb = parse_block_contents(p)?;
        p.expect(&TokenKind::RBrace, "'}'");
        // Braces included -- `parse_block_contents` only sees the interior,
        // so it cannot compute this itself.
        cb.span = start.to(p.last_span());
        Some(cb)
    })
}

pub fn parse_block_contents(p: &mut Parser) -> Option<CodeblockExpr> {
    let start = p.peek_span();
    let mut statements = Vec::new();
    let tail = loop {
        if p.check(&TokenKind::RBrace) || p.is_eof() {
            break None;
        }
        let mark = p.mark();
        if let Some(expr) = parse_statement_leading_expression(p)
            && (p.check(&TokenKind::RBrace) || p.is_eof())
        {
            break Some(Box::new(expr));
        }
        p.reset(mark);
        match parse_statement(p) {
            Some(stmt) => statements.push(stmt),
            None => crate::parser::recovery::synchronize_to_statement_boundary(p),
        }
    };
    let span = start.to(p.last_span());
    Some(CodeblockExpr {
        statements,
        tail,
        span,
    })
}

#[cfg(test)]
mod nesting_tests;
#[cfg(test)]
mod tests;
