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
    /// `..` -- no end, ever. `..` is the *inference* operator: it says
    /// "work this side out", so `a..b` is a syntax error in every position
    /// (`ParseErrorKind::OpenRangeHasEnd`), not a third spelling alongside
    /// `..<`/`..=`. What it infers depends on where it appears -- a slice's
    /// own container length, a `match` arm's unmatched remainder, or, in
    /// ordinary expression position, the element type's own domain limit
    /// through `core::range::Bounded`.
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
/// (`omega_parser::ast::expression::match_expr::Pattern::Range`), and
/// ordinary expression position
/// (`omega_parser::ast::expression::Expression::Range`), which builds a real
/// `core::range::Range<T>` value.
///
/// - `..=b`  -- `[MIN, b]`
/// - `a..=b` -- `[a, b]`
/// - `..<b`  -- `[MIN, b)`
/// - `a..<b` -- `[a, b)`
/// - `..`    -- fully open; both ends inferred
/// - `a..`   -- `[a, inferred]`
///
/// `start` is independently optional from `end` in every case above.
///
/// What is *not* in that list is an end bound following bare `..`, in either
/// of its shapes: neither `a..b` nor `..5` parses anywhere
/// (`ParseErrorKind::OpenRangeHasEnd`). `..` is the spelling for "no bound
/// written on this side", so an end after it is a contradiction. Writing an
/// end at all -- with a start, as `a..<b`, or without one, as `..<b` -- uses
/// `..<`/`..=`, which are separate tokens rather than `..` with something
/// appended.
///
/// The grammar is identical in all three positions; what differs is only
/// what an *inferred* side resolves to, which is the consuming position's
/// own question (see `RangeEnd::Open`).
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
