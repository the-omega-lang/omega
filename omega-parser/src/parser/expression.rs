use crate::ast::expression::{
    Expression, ExpressionNode, address_of::AddressOfExpr, array_literal::ArrayLiteralExpr,
    assignment::AssignmentExpr, binary_op::{BinaryOp, BinaryOpExpr}, bool_literal::BoolExpr,
    char_literal::CharExpr, codeblock::CodeblockExpr, deref::DerefExpr,
    field_access::FieldAccessExpr, function_call::FunctionCallExpr, if_expr::IfExpr,
    incr_decr::{DecrementExpr, IncrementExpr}, index::IndexExpr, negate::NegateExpr,
    slice::SliceExpr, string::StringExpr,
};
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::{Parser, macro_syntax::parse_macro_invocation, parse_path, statement::parse_statement};

/// The full expression grammar's single entry point. A deliberate hybrid,
/// not one generic precedence-climbing loop for everything: assignment
/// (right-associative, recurses into the whole grammar again on the RHS)
/// and comparison (non-associative -- at most one `==`/`!=`/`<`/`<=`/`>`/
/// `>=`; `a < b < c` must be a parse error, not `(a < b) < c`) are explicit
/// outer layers matching their exact non-standard semantics; only the
/// genuinely standard left-associative additive/multiplicative tiers use
/// real precedence-climbing (see `parse_additive`/`parse_multiplicative`).
/// Precedence, loosest to tightest: assignment, comparison, additive,
/// multiplicative, prefix unary, postfix, primary.
pub fn parse_expression(p: &mut Parser) -> Option<ExpressionNode> {
    parse_assignment(p)
}

fn parse_assignment(p: &mut Parser) -> Option<ExpressionNode> {
    let target = parse_comparison(p)?;
    if !p.check(&TokenKind::Eq) {
        return Some(target);
    }
    p.advance();
    let value = parse_expression(p)?;
    let span = target.span.to(value.span);
    Some(ExpressionNode {
        expression: Expression::Assignment(Box::new(AssignmentExpr { target, value: Box::new(value) })),
        span,
    })
}

/// `== != < <= > >=`, non-associative: at most one comparison is matched
/// here (not a loop), matching Rust's own rule that `a < b < c` must be
/// parenthesized rather than either chaining or silently meaning
/// `(a < b) < c`.
fn parse_comparison(p: &mut Parser) -> Option<ExpressionNode> {
    let left = parse_additive(p)?;
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
    let right = parse_additive(p)?;
    let span = left.span.to(right.span);
    Some(ExpressionNode { expression: binary_op_expr(left, op, right), span })
}

fn parse_additive(p: &mut Parser) -> Option<ExpressionNode> {
    let mut left = parse_multiplicative(p)?;
    loop {
        let op = match p.peek() {
            TokenKind::Plus => BinaryOp::Add,
            TokenKind::Minus => BinaryOp::Sub,
            _ => break,
        };
        p.advance();
        let right = parse_multiplicative(p)?;
        let span = left.span.to(right.span);
        left = ExpressionNode { expression: binary_op_expr(left, op, right), span };
    }
    Some(left)
}

fn parse_multiplicative(p: &mut Parser) -> Option<ExpressionNode> {
    let mut left = parse_unary(p)?;
    loop {
        let op = match p.peek() {
            TokenKind::Star => BinaryOp::Mul,
            TokenKind::Slash => BinaryOp::Div,
            TokenKind::Percent => BinaryOp::Rem,
            _ => break,
        };
        p.advance();
        let right = parse_unary(p)?;
        let span = left.span.to(right.span);
        left = ExpressionNode { expression: binary_op_expr(left, op, right), span };
    }
    Some(left)
}

fn binary_op_expr(left: ExpressionNode, op: BinaryOp, right: ExpressionNode) -> Expression {
    Expression::BinaryOp(Box::new(BinaryOpExpr { left, op, right }))
}

/// Binds tighter than the arithmetic operators and assignment, but looser
/// than postfix: `*base`/`&base`/`-base`. So `*p.f` is `*(p.f)` (postfix
/// runs first, see `parse_postfix`), while `(*p).f` needs explicit parens,
/// and `-a * b` is `(-a) * b` -- matching C/Rust precedence. `++`/`--` need
/// no special ordering against `+`/`-` here (unlike the old char-by-char
/// grammar, which had to try the two-char forms first) -- the lexer already
/// maximal-munched them into their own distinct token kinds, so there's no
/// way for `--x` to be mistaken for two stacked unary minuses at this
/// layer. Right-associative via plain recursion: `prefix.repeated()`'s old
/// fold-right becomes just "the operand is itself a `parse_unary` call."
fn parse_unary(p: &mut Parser) -> Option<ExpressionNode> {
    let start = p.peek_span();
    enum Prefix {
        Deref,
        AddressOf,
        Negate,
        Increment,
        Decrement,
    }
    let prefix = match p.peek() {
        TokenKind::PlusPlus => Prefix::Increment,
        TokenKind::MinusMinus => Prefix::Decrement,
        TokenKind::Star => Prefix::Deref,
        TokenKind::Amp => Prefix::AddressOf,
        TokenKind::Minus => Prefix::Negate,
        _ => return parse_postfix(p),
    };
    p.advance();
    let base = parse_unary(p)?;
    let span = start.to(base.span);
    let expression = match prefix {
        Prefix::Deref => Expression::Deref(Box::new(DerefExpr { base })),
        Prefix::AddressOf => Expression::AddressOf(Box::new(AddressOfExpr { base })),
        Prefix::Negate => Expression::Negate(Box::new(NegateExpr { base })),
        Prefix::Increment => Expression::Increment(Box::new(IncrementExpr { base })),
        Prefix::Decrement => Expression::Decrement(Box::new(DecrementExpr { base })),
    };
    Some(ExpressionNode { expression, span })
}

/// Binds tightest: `.field`, `[index]`/`[a..b]`, `(args)`, left-associative
/// via a post-primary loop (the old grammar's `foldl_with(postfix.repeated())`
/// translates directly).
fn parse_postfix(p: &mut Parser) -> Option<ExpressionNode> {
    let mut expr = parse_primary(p)?;
    loop {
        match p.peek() {
            TokenKind::Dot => {
                p.advance();
                let field_span = p.peek_span();
                let field = p.expect_ident()?;
                let span = expr.span.to(field_span);
                expr = ExpressionNode {
                    expression: Expression::FieldAccess(Box::new(FieldAccessExpr { base: expr, field })),
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

/// `base[index]` vs `base[start..end]` (`start`/`end` each independently
/// optional) -- told apart by the presence of a bare `..`: if the token
/// right after `[` is `..`, there's no start bound; otherwise one mandatory
/// expression is parsed first and *then* checked for a following `..` --
/// either way, no real backtracking is needed (unlike the old grammar's
/// `choice((range, item))`, which had to speculatively try the whole range
/// shape first): the `..` decides which shape this is only once we've seen
/// it, and everything up to that point is identical for both.
fn parse_index_or_slice(p: &mut Parser, base: ExpressionNode) -> Option<ExpressionNode> {
    p.advance(); // '['
    if p.eat(&TokenKind::DotDot) {
        let end = parse_optional_slice_bound(p)?;
        return finish_slice(p, base, None, end);
    }
    let first = parse_expression(p)?;
    if p.eat(&TokenKind::DotDot) {
        let end = parse_optional_slice_bound(p)?;
        finish_slice(p, base, Some(first), end)
    } else {
        let close_span = p.peek_span();
        p.expect(&TokenKind::RBracket, "']'");
        let span = base.span.to(close_span);
        Some(ExpressionNode { expression: Expression::Index(Box::new(IndexExpr { base, index: first })), span })
    }
}

fn parse_optional_slice_bound(p: &mut Parser) -> Option<Option<ExpressionNode>> {
    if p.check(&TokenKind::RBracket) { Some(None) } else { parse_expression(p).map(Some) }
}

fn finish_slice(
    p: &mut Parser,
    base: ExpressionNode,
    start: Option<ExpressionNode>,
    end: Option<ExpressionNode>,
) -> Option<ExpressionNode> {
    let close_span = p.peek_span();
    p.expect(&TokenKind::RBracket, "']'");
    let span = base.span.to(close_span);
    Some(ExpressionNode { expression: Expression::Slice(Box::new(SliceExpr { base, start, end })), span })
}

/// `callee(args)` -- comma-separated, no trailing comma tolerated (matching
/// the old grammar's plain `separated_by`, which has the same rule: a
/// trailing comma leaves nothing for the next iteration to parse, reported
/// as an ordinary "expected an expression" error there rather than silently
/// accepted).
fn parse_call(p: &mut Parser, callee: ExpressionNode) -> Option<ExpressionNode> {
    p.advance(); // '('
    let mut args = Vec::new();
    if !p.check(&TokenKind::RParen) {
        loop {
            args.push(parse_expression(p)?);
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
    }
    let close_span = p.peek_span();
    p.expect(&TokenKind::RParen, "')'");
    let span = callee.span.to(close_span);
    Some(ExpressionNode { expression: Expression::FunctionCall(FunctionCallExpr { callee: Box::new(callee), args }), span })
}

/// The atom tier. Order matters and matches the old grammar's `choice`
/// exactly: `Bool` is tried before `Path` (`true`/`false` are keywords in
/// this position, not identifiers -- see `lexer::TokenKind`'s doc comment
/// on why they're global keywords now), and macro invocation is tried
/// before `Path` (an identifier immediately followed by `!` must not be
/// parsed as a bare path with `!(...)` left dangling).
fn parse_primary(p: &mut Parser) -> Option<ExpressionNode> {
    let start = p.peek_span();
    match p.peek() {
        TokenKind::LParen => {
            p.advance();
            let inner = parse_expression(p)?;
            p.expect(&TokenKind::RParen, "')'");
            // Deliberately keeps `inner`'s own span, not one extended to
            // cover the parens -- matches the old grammar, which never
            // re-wrapped a parenthesized expression's span either.
            Some(inner)
        }
        TokenKind::LBrace => {
            let cb = parse_codeblock(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode { expression: Expression::Codeblock(cb), span })
        }
        TokenKind::If => {
            let if_expr = parse_if_expr(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode { expression: Expression::If(Box::new(if_expr)), span })
        }
        TokenKind::LBracket => parse_array_literal(p),
        TokenKind::Number(_) => {
            let TokenKind::Number(n) = p.advance().kind else { unreachable!() };
            Some(ExpressionNode { expression: Expression::Number(n), span: start })
        }
        TokenKind::Str(_) => {
            let TokenKind::Str(s) = p.advance().kind else { unreachable!() };
            Some(ExpressionNode { expression: Expression::String(StringExpr(s)), span: start })
        }
        TokenKind::Char(_) => {
            let TokenKind::Char(c) = p.advance().kind else { unreachable!() };
            Some(ExpressionNode { expression: Expression::Char(CharExpr(c)), span: start })
        }
        TokenKind::True => {
            p.advance();
            Some(ExpressionNode { expression: Expression::Bool(BoolExpr(true)), span: start })
        }
        TokenKind::False => {
            p.advance();
            Some(ExpressionNode { expression: Expression::Bool(BoolExpr(false)), span: start })
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Bang) => {
            let inv = parse_macro_invocation(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode { expression: Expression::MacroInvocation(inv), span })
        }
        TokenKind::Ident(_) => {
            let path = parse_path(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode { expression: Expression::Path(path), span })
        }
        _ => {
            p.error(ParseErrorKind::Expected { expected: "an expression", found: p.peek().describe() });
            None
        }
    }
}

/// `[e1, e2, ...]` -- same "no trailing comma" rule as `parse_call`.
fn parse_array_literal(p: &mut Parser) -> Option<ExpressionNode> {
    let start = p.peek_span();
    p.advance(); // '['
    let mut elements = Vec::new();
    if !p.check(&TokenKind::RBracket) {
        loop {
            elements.push(parse_expression(p)?);
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
    }
    p.expect(&TokenKind::RBracket, "']'");
    let span = start.to(p.last_span());
    Some(ExpressionNode { expression: Expression::ArrayLiteral(ArrayLiteralExpr { elements }), span })
}

/// `if cond { ... } else if cond { ... } else { ... }`.
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
    Some(IfExpr { branches, else_branch })
}

/// `cond { ... }` -- the leading `if`/`else if` keyword itself is always
/// consumed by the caller before this runs.
fn parse_if_branch_body(p: &mut Parser) -> Option<(ExpressionNode, CodeblockExpr)> {
    let condition = parse_expression(p)?;
    let body = parse_codeblock(p)?;
    Some((condition, body))
}

/// `{ stmt; stmt; ... tail }` -- at every position, tries the tail
/// interpretation first (does a full expression parse here *and* get
/// immediately followed by `}`?), falling back to "one ordinary statement"
/// only if that fails -- matching the old grammar's own `tail_only`-before-
/// `one_more_stmt` ordering exactly (needed so a trailing `if`/`{}`
/// expression meant as the block's value isn't instead swallowed as just
/// another statement, silently discarding it). `mark`/`reset` is the
/// backtracking primitive this genuinely needs: an ordinary statement can
/// itself start by parsing the very same expression grammar (e.g. an
/// expression-statement), so there is no cheaper, backtracking-free way to
/// tell "this is the tail" from "this is a statement" apart than trying the
/// expression interpretation and checking what follows.
pub fn parse_codeblock(p: &mut Parser) -> Option<CodeblockExpr> {
    p.expect(&TokenKind::LBrace, "'{'");
    let mut statements = Vec::new();
    let tail = loop {
        if p.check(&TokenKind::RBrace) || p.is_eof() {
            break None;
        }
        let mark = p.mark();
        if let Some(expr) = parse_expression(p)
            && p.check(&TokenKind::RBrace)
        {
            break Some(Box::new(expr));
        }
        p.reset(mark);
        match parse_statement(p) {
            Some(stmt) => statements.push(stmt),
            None => crate::parser::recovery::synchronize_to_statement_boundary(p),
        }
    };
    p.expect(&TokenKind::RBrace, "'}'");
    Some(CodeblockExpr { statements, tail })
}
