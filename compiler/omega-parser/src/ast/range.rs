use crate::ast::expression::ExpressionNode;
use crate::diagnostics::Span;

/// One range, in the single grammar shared by slicing (`base[range]`) and
/// match range-patterns (`omega_parser::ast::expression::match_expr::Pattern::Range`).
/// There is no plain two-dot `..` in this language at all -- every range is
/// spelled either `...` (inclusive, `inclusive: true`) or `..<`
/// (exclusive-end, `inclusive: false`), with `start`/`end` each
/// independently optional:
///
/// - `...`   -- the full domain (both ends open)
/// - `a...`  -- `[a, MAX]`
/// - `...b`  -- `[MIN, b]`
/// - `a...b` -- `[a, b]`
/// - `..<b`  -- `[MIN, b)`
/// - `a..<b` -- `[a, b)`
///
/// `..<` always requires an explicit end (`a..<` and bare `..<` are parse
/// errors, `ParseErrorKind::ExclusiveRangeMissingEnd`) -- an open-ended
/// exclusive range has nothing to exclude.
#[derive(Debug, Clone)]
pub struct RangeExpr {
    pub start: Option<ExpressionNode>,
    pub end: Option<ExpressionNode>,
    pub inclusive: bool,
    pub span: Span,
}
