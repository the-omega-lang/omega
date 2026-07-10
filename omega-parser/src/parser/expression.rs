use crate::ast::expression::{
    Expression, ExpressionNode, address_of::AddressOfExpr, array_literal::ArrayLiteralExpr,
    assignment::AssignmentExpr, binary_op::{BinaryOp, BinaryOpExpr}, bool_literal::BoolExpr,
    char_literal::CharExpr, codeblock::CodeblockExpr, deref::DerefExpr,
    field_access::FieldAccessExpr, function_call::FunctionCallExpr, if_expr::IfExpr,
    incr_decr::{DecrementExpr, IncrementExpr}, index::IndexExpr,
    match_expr::{MatchArm, MatchExpr, Pattern}, negate::NegateExpr, slice::SliceExpr,
    string::StringExpr, struct_literal::{StructLiteralExpr, StructLiteralField},
};
use crate::ast::range::RangeExpr;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::{Parser, macro_syntax::parse_macro_invocation, statement::parse_statement};

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
        AddressOf { mutable: bool },
        Negate,
        Increment,
        Decrement,
    }
    let prefix = match p.peek() {
        TokenKind::PlusPlus => Prefix::Increment,
        TokenKind::MinusMinus => Prefix::Decrement,
        TokenKind::Star => Prefix::Deref,
        // `&mut` -- `mut` is a contextual keyword here (see
        // `lexer::TokenKind`'s doc comment), checked by comparing an
        // already-lexed `Ident`'s text, exactly like `self`/pointer types'
        // own `*mut` check.
        TokenKind::Amp if matches!(p.peek_at(1), TokenKind::Ident(name) if name == "mut") => {
            Prefix::AddressOf { mutable: true }
        }
        TokenKind::Amp => Prefix::AddressOf { mutable: false },
        TokenKind::Minus => Prefix::Negate,
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
        Prefix::AddressOf { mutable } => Expression::AddressOf(Box::new(AddressOfExpr { base, mutable })),
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

/// `base[index]` vs `base[range]` -- told apart by whether a range operator
/// (`...`/`..<`) appears right after `[` (no start bound) or right after one
/// mandatory expression is parsed first -- either way, no real backtracking
/// is needed (unlike the old grammar's `choice((range, item))`, which had to
/// speculatively try the whole range shape first): the operator decides
/// which shape this is only once we've seen it, and everything up to that
/// point is identical for both. See `RangeExpr`'s doc comment for the range
/// grammar itself, shared verbatim with match patterns.
fn parse_index_or_slice(p: &mut Parser, base: ExpressionNode) -> Option<ExpressionNode> {
    p.advance(); // '['
    if matches!(p.peek(), TokenKind::DotDotDot | TokenKind::DotDotLt) {
        let op_span = p.peek_span();
        let range = parse_range_tail(p, None, op_span, &TokenKind::RBracket)?;
        return finish_slice(p, base, range);
    }
    let first = p.allow_struct_literals(parse_expression)?;
    if matches!(p.peek(), TokenKind::DotDotDot | TokenKind::DotDotLt) {
        let op_span = p.peek_span();
        let range = parse_range_tail(p, Some(first), op_span, &TokenKind::RBracket)?;
        return finish_slice(p, base, range);
    }
    let close_span = p.peek_span();
    p.expect(&TokenKind::RBracket, "']'");
    let span = base.span.to(close_span);
    Some(ExpressionNode { expression: Expression::Index(Box::new(IndexExpr { base, index: first })), span })
}

/// Consumes the range operator at the parser's current position (`...` or
/// `..<` -- the caller has already confirmed one is here) and parses the
/// rest of the shared range grammar. `terminator` is whatever token means
/// "no end bound follows" in the caller's context (`]` for a slice, `=>` for
/// a match pattern). `..<` always requires an explicit end
/// (`ExclusiveRangeMissingEnd` otherwise, per `RangeExpr`'s doc comment).
fn parse_range_tail(
    p: &mut Parser,
    start: Option<ExpressionNode>,
    op_span: Span,
    terminator: &TokenKind,
) -> Option<RangeExpr> {
    let inclusive = match p.peek() {
        TokenKind::DotDotDot => true,
        TokenKind::DotDotLt => false,
        _ => unreachable!("caller already confirmed a range operator is here"),
    };
    p.advance();
    let end = if p.check(terminator) { None } else { Some(p.allow_struct_literals(parse_expression)?) };
    if !inclusive && end.is_none() {
        p.error_at(op_span, ParseErrorKind::ExclusiveRangeMissingEnd);
        return None;
    }
    let lo = start.as_ref().map(|s| s.span).unwrap_or(op_span);
    let hi = end.as_ref().map(|e| e.span).unwrap_or(op_span);
    Some(RangeExpr { start, end, inclusive, span: lo.to(hi) })
}

fn finish_slice(p: &mut Parser, base: ExpressionNode, range: RangeExpr) -> Option<ExpressionNode> {
    let close_span = p.peek_span();
    p.expect(&TokenKind::RBracket, "']'");
    let span = base.span.to(close_span);
    Some(ExpressionNode { expression: Expression::Slice(Box::new(SliceExpr { base, range })), span })
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
            args.push(p.allow_struct_literals(parse_expression)?);
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
            let inner = p.allow_struct_literals(parse_expression)?;
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
        TokenKind::Match => {
            let match_expr = parse_match_expr(p)?;
            let span = start.to(p.last_span());
            Some(ExpressionNode { expression: Expression::Match(Box::new(match_expr)), span })
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
            Some(ExpressionNode { expression: Expression::Path(path), span })
        }
        _ => {
            p.error(ParseErrorKind::Expected { expected: "an expression", found: p.peek().describe() });
            None
        }
    }
}

/// A path in expression position, with speculative support for explicit
/// generic arguments on one segment: `Optional<u32>::Some`,
/// `MyNode<i32> { ... }`. Unlike type position -- where `<` can only mean
/// generic arguments -- a `<` here is usually the comparison operator
/// (`a < b`), so the generic reading is only *committed* when what follows
/// the closing `>` proves it: another `::` segment, or a `{` opening a
/// struct literal where one is allowed. (`a < b > c` isn't valid syntax
/// anyway -- comparison is non-associative, see `parse_comparison` -- so
/// nothing parseable is ever stolen by committing on those two tokens.)
/// Anything less conclusive resets and leaves the `<` for the comparison
/// tier, exactly like `recover_restricted_struct_literal`'s mark/reset
/// discipline -- abandoned speculation never leaks errors.
fn parse_expr_path(p: &mut Parser) -> Option<crate::ast::identifier::ExprPath> {
    use crate::ast::identifier::ExprPath;

    let head = p.expect_ident()?;
    let mut path = crate::ast::identifier::Path { head, tail: Vec::new() };
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

    Some(ExprPath { path, generic_args, args_at })
}

/// The speculative `<Type, ...>` attempt behind `parse_expr_path` -- returns
/// the parsed arguments only when the commit rule holds (see its doc
/// comment), resetting the parser to just before the `<` otherwise.
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
    if !p.eat(&TokenKind::Gt) {
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

/// A struct literal written where they're restricted (`if Name { ... }` --
/// see `Parser::restrict_struct_literals`): normally the `{` simply starts
/// the statement's body and the path stands alone, but when what follows
/// can *only* be read as a struct literal, silently mis-parsing it as
/// "condition, then body" would bury the user in nonsense errors inside
/// the "body". So this speculatively parses the literal and keeps it --
/// with one precise `StructLiteralNotAllowedHere` error -- exactly when
/// the token after its closing `}` proves the literal reading (another
/// `{` for the real body, a projection, or an operator continuing the
/// condition; none of these can follow a completed `if cond { body }`
/// mid-statement). Anything less conclusive resets and lets the ordinary
/// "path, then body" interpretation proceed untouched.
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

/// `Name { field: value; ... }` -- the caller has already parsed `path` and
/// confirmed both that a `{` follows and that a struct literal is allowed
/// here (see `Parser::struct_literals_allowed`). Field initializers are
/// `;`-terminated, deliberately mirroring struct *definition* syntax
/// (`field: Type;` there, `field: value;` here).
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
        p.expect(&TokenKind::Colon, "':'");
        // Inside the literal's braces, a nested struct literal is
        // unambiguous again even if this one sits in condition position.
        let value = p.allow_struct_literals(parse_expression)?;
        p.expect_terminator(&TokenKind::Semi, "';'");
        fields.push(StructLiteralField { name, name_span, value });
    }
    if !p.check(&TokenKind::RBrace) {
        p.error(ParseErrorKind::Expected {
            expected: "a field initializer (`name: value;`) or '}'",
            found: p.peek().describe(),
        });
        return None;
    }
    p.advance(); // '}'
    let span = start.to(p.last_span());
    Some(ExpressionNode { expression: Expression::StructLiteral(StructLiteralExpr { path, fields }), span })
}

/// `[e1, e2, ...]` -- same "no trailing comma" rule as `parse_call`.
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
/// consumed by the caller before this runs. The condition parses with
/// struct literals restricted: `if flag { ... }` must mean "condition
/// `flag`, then the branch body," never a `flag { ... }` literal (see
/// `Parser::restrict_struct_literals`).
fn parse_if_branch_body(p: &mut Parser) -> Option<(ExpressionNode, CodeblockExpr)> {
    let condition = p.restrict_struct_literals(parse_expression)?;
    let body = parse_codeblock(p)?;
    Some((condition, body))
}

/// `match scrutinee { pattern => body, ... } else { ... }`. The scrutinee
/// parses with struct literals restricted, exactly like an `if` condition
/// (`parse_if_branch_body`) -- `match flag { ... }` must mean "scrutinee
/// `flag`, then the arm list," never a `flag { ... }` literal.
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
    let else_branch = if p.eat(&TokenKind::Else) { Some(parse_codeblock(p)?) } else { None };
    let span = start.to(p.last_span());
    Some(MatchExpr { scrutinee, arms, else_branch, span })
}

/// `pattern => body` -- arms are comma-separated with an optional trailing
/// comma, uniformly regardless of whether `body` is a bare expression or a
/// `{ ... }` block (simpler than Rust's "comma optional after a block"
/// special case).
fn parse_match_arm(p: &mut Parser) -> Option<MatchArm> {
    let start = p.peek_span();
    let pattern = parse_pattern(p)?;
    p.expect(&TokenKind::FatArrow, "'=>'");
    let body = p.allow_struct_literals(parse_expression)?;
    let span = start.to(body.span);
    Some(MatchArm { pattern, body, span })
}

/// One pattern: a range (leading `...`/`..<`, or one expression followed by
/// `...`/`..<`), or else that one expression stands alone as
/// `Pattern::Value` -- a literal or an `Enum::Variant` path, told apart by
/// analysis (see `Pattern`'s doc comment), not here. Reuses
/// `parse_range_tail` verbatim (terminated by `=>` instead of a slice's
/// `]`), so the range grammar is defined in exactly one place.
fn parse_pattern(p: &mut Parser) -> Option<Pattern> {
    if matches!(p.peek(), TokenKind::DotDotDot | TokenKind::DotDotLt) {
        let op_span = p.peek_span();
        let range = parse_range_tail(p, None, op_span, &TokenKind::FatArrow)?;
        return Some(Pattern::Range(range));
    }
    let value = p.allow_struct_literals(parse_expression)?;
    if matches!(p.peek(), TokenKind::DotDotDot | TokenKind::DotDotLt) {
        let op_span = p.peek_span();
        let range = parse_range_tail(p, Some(value), op_span, &TokenKind::FatArrow)?;
        return Some(Pattern::Range(range));
    }
    Some(Pattern::Value(value))
}

#[cfg(test)]
mod tests {
    use crate::SourceModule;
    use crate::ast::expression::Expression;
    use crate::ast::statement::{Item, Statement};
    use crate::diagnostics::ParseErrorKind;

    /// The statements of `source`'s first (and only) function definition.
    fn body_statements(source: &str) -> Vec<Statement> {
        let module = SourceModule::parse(source).expect("source must parse");
        let Item::FunctionDefinition(f) = &module.nodes[0].item else {
            panic!("first item must be a function");
        };
        f.codeblock.statements.iter().map(|s| s.statement.clone()).collect()
    }

    #[test]
    fn struct_literal_parses_with_fields_in_order() {
        let stmts = body_statements("f() => i32 { v := Vec2 { x: 1; y: 2; }; v.x }");
        let Statement::Walrus(w) = &stmts[0] else { panic!("expected a walrus statement") };
        let Expression::StructLiteral(lit) = &w.value.expression else {
            panic!("expected a struct literal value")
        };
        assert_eq!(lit.path.path.head.as_ref(), "Vec2");
        let names: Vec<&str> = lit.fields.iter().map(|f| f.name.as_ref()).collect();
        assert_eq!(names, ["x", "y"]);
    }

    #[test]
    fn generic_args_commit_on_path_continuation() {
        // `Optional<u32>::Some { ... }` -- the `::` after `>` proves the
        // generic reading; the literal's path carries the args on segment 0.
        let stmts = body_statements("f() => void { a := Optional<u32>::Some { value: 10; }; }");
        let Statement::Walrus(w) = &stmts[0] else { panic!("expected a walrus statement") };
        let Expression::StructLiteral(lit) = &w.value.expression else {
            panic!("expected a struct literal value")
        };
        assert_eq!(lit.path.path.head.as_ref(), "Optional");
        assert_eq!(lit.path.path.tail[0].as_ref(), "Some");
        assert_eq!(lit.path.generic_args.len(), 1);
        assert_eq!(lit.path.args_at, 0);
    }

    #[test]
    fn generic_args_do_not_steal_comparisons() {
        // `a < b` followed by something that is neither `::` nor `{` must
        // stay a comparison -- including the nasty `f(a < b, c > d)` shape,
        // where a C++-style greedy reading would see `a<b, c>(d)`.
        let stmts = body_statements("f() => void { x := a < b; g(a < b, c > d); }");
        let Statement::Walrus(w) = &stmts[0] else { panic!("expected a walrus statement") };
        assert!(matches!(w.value.expression, Expression::BinaryOp(_)));
        let Statement::Expression(call) = &stmts[1] else { panic!("expected a call statement") };
        let Expression::FunctionCall(call) = &call.expression else { panic!("expected a call") };
        assert_eq!(call.args.len(), 2);
    }

    #[test]
    fn enum_with_header_bodies_and_functions_parses() {
        let source = r#"
            enum MyCoolEnum(tag: i16, description: *u8) {
                Bad(-1, "bad"),
                First(0, "first") { message: *u8; },
                Second(1, "second") {
                    number: u64;
                    decimal: f64;
                }
                Third(2, "third");

                print_description(self) => void { puts(self.description); }
                make() => MyCoolEnum { MyCoolEnum::Third }
            }
        "#;
        let module = SourceModule::parse(source).expect("enum must parse");
        let Item::Enum(e) = &module.nodes[0].item else { panic!("expected an enum item") };
        assert_eq!(e.ident.as_ref(), "MyCoolEnum");
        assert_eq!(e.header.len(), 2);
        assert_eq!(e.header[0].ident.as_ref(), "tag");
        let names: Vec<&str> = e.variants.iter().map(|v| v.ident.as_ref()).collect();
        assert_eq!(names, ["Bad", "First", "Second", "Third"]);
        assert_eq!(e.variants[2].fields.len(), 2);
        assert_eq!(e.functions.len(), 2);
    }

    #[test]
    fn enum_function_without_variant_terminator_reports_dedicated_error() {
        let errors = SourceModule::parse(
            "enum E { First, Second do_thing(self) => void { } }",
        )
        .expect_err("must not parse");
        assert!(errors.iter().any(|e| matches!(e.kind, ParseErrorKind::EnumFunctionBeforeSemi)));
    }

    #[test]
    fn enum_in_statement_position_reports_dedicated_error() {
        let errors = SourceModule::parse("f() => void { enum E { A } }").expect_err("must not parse");
        assert!(errors.iter().any(|e| matches!(e.kind, ParseErrorKind::EnumNotAllowedHere)));
    }

    #[test]
    fn condition_position_reads_brace_as_body_not_literal() {
        // `flag { ... }` in a `while` condition must be "condition `flag`,
        // then the body" -- including when the body's first statement is a
        // declaration (`x: i32;`), which is field-initializer-shaped.
        let stmts = body_statements("f() => void { while flag { x: i32; } }");
        let Statement::While(w) = &stmts[0] else { panic!("expected a while statement") };
        assert!(matches!(w.condition.expression, Expression::Path(_)));
        assert!(matches!(w.body.statements[0].statement, Statement::Declaration(_)));
    }

    #[test]
    fn unambiguous_literal_in_condition_reports_dedicated_error() {
        // The `.x > 0` after the closing `}` proves the literal reading --
        // one precise error, not a cascade from mis-parsing the "body".
        let errors = SourceModule::parse("f() => void { if Vec2 { x: 1; }.x > 0 { g(); } }")
            .expect_err("must not parse");
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0].kind, ParseErrorKind::StructLiteralNotAllowedHere));
    }

    #[test]
    fn parenthesized_literal_in_condition_parses() {
        // The suggested fix for the case above must itself parse. (The
        // trailing `done();` keeps the `if` in statement position rather
        // than the block's tail.)
        let stmts = body_statements("f() => void { if (Vec2 { x: 1; }).x > 0 { g(); } done(); }");
        assert!(matches!(stmts[0], Statement::Expression(_)));
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn literal_inside_call_arguments_in_condition_parses() {
        // Bracketed sub-contexts lift the restriction: the `{` inside
        // `check(...)`'s arguments can't be the statement's body.
        let stmts = body_statements("f() => void { if check(Vec2 { x: 1; }) { g(); } done(); }");
        assert!(matches!(stmts[0], Statement::Expression(_)));
        assert_eq!(stmts.len(), 2);
    }
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
    // Inside the block's own braces, struct literals are unambiguous again
    // regardless of what position the block itself sits in.
    p.allow_struct_literals(|p| {
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
    })
}
