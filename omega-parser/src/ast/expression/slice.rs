use crate::ast::expression::ExpressionNode;

/// `base[start..end]` (and the `start..`/`..end`/`..` variants, `start`/`end`
/// each independently optional) -- unlike a plain `Index`, this never
/// produces a single element: it produces a new slice (fat pointer) over a
/// sub-range of `base`. Parsed as a distinct postfix form from `Index`
/// rather than reusing it with an optional end bound, since the two mean
/// entirely different things (one element vs. a sub-range) and should be
/// told apart as early as possible rather than disambiguated downstream.
#[derive(Debug, Clone)]
pub struct SliceExpr {
    pub base: ExpressionNode,
    pub start: Option<ExpressionNode>,
    pub end: Option<ExpressionNode>,
}
