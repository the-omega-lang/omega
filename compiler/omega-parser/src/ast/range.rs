use crate::ast::expression::ExpressionNode;
use crate::diagnostics::Span;

/// A range's own end, in the one shape a caller can spell it: `..=end`
/// (inclusive), `..<end` (exclusive), or bare `..` (no end at all, ever
/// -- the only spelling that's legal with nothing written). Making this
/// an enum instead of `end: Option<Expr> + inclusive: bool` (the old
/// shape) means an inclusive/exclusive range with no end, or an open
/// range with one, are no longer representable at all rather than
/// merely rejected by a runtime check.
#[derive(Debug, Clone)]
pub enum RangeEnd {
    /// `..=end`
    Inclusive(ExpressionNode),
    /// `..<end` -- always requires an explicit end (`a..<` and bare `..<`
    /// are parse errors, `ParseErrorKind::RangeMissingEnd`, which also
    /// covers `..=`'s identical requirement) -- an open-ended exclusive
    /// range has nothing to exclude.
    Exclusive(ExpressionNode),
    /// `..` -- no end, ever. What it actually means is inferred entirely
    /// by whichever position consumes it: a slice's own container length,
    /// a `match` arm's own unmatched remainder, or a range-driven `for`
    /// loop's own element-type domain.
    Open,
}

impl RangeEnd {
    pub fn end_expr(&self) -> Option<&ExpressionNode> {
        match self {
            Self::Inclusive(e) | Self::Exclusive(e) => Some(e),
            Self::Open => None,
        }
    }

    pub fn inclusive(&self) -> bool {
        matches!(self, Self::Inclusive(_) | Self::Open)
    }
}

/// One range, in the single grammar shared by slicing (`base[range]`),
/// `match` range-patterns
/// (`omega_parser::ast::expression::match_expr::Pattern::Range`), and --
/// legal only as a `for` loop's own direct iterator source -- a
/// standalone range expression
/// (`omega_parser::ast::expression::Expression::Range`).
///
/// - `..=b`  -- `[MIN, b]`
/// - `a..=b` -- `[a, b]`
/// - `..<b`  -- `[MIN, b)`
/// - `a..<b` -- `[a, b)`
/// - `..`    -- fully open; inferred both ends
/// - `a..`   -- `[a, inferred]`
///
/// `start` is independently optional from `end` in every case above.
/// There is no way to spell a fully-open *inclusive* range the old
/// `...`/`a...` way did -- that shape is now exclusively bare `..`/`a..`.
#[derive(Debug, Clone)]
pub struct RangeExpr {
    pub start: Option<ExpressionNode>,
    pub end: RangeEnd,
    pub span: Span,
}

impl RangeExpr {
    /// Whether this is bare `..` -- nothing written on either side. The
    /// one shape that means "catch-all" in a `match` arm, and the one
    /// shape a `for` loop's own range-missing-start diagnostic rejects
    /// (see `Analyzer::analyze_for`).
    pub fn is_catch_all(&self) -> bool {
        self.start.is_none() && matches!(self.end, RangeEnd::Open)
    }

    pub fn inclusive(&self) -> bool {
        self.end.inclusive()
    }
}
