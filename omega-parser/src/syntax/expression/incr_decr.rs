use crate::prelude::ExpressionNode;

/// `++base` -- sugar for "add one and assign back," but not represented that
/// way syntactically: `base` isn't guaranteed to be a place at this level
/// (same rationale as `AddressOfExpr`/`NegateExpr`), so analysis is what
/// validates it and performs the actual desugaring once it knows `base`'s
/// resolved type (see `Analyzer::analyze_incr_decr` -- the "+1"/"-1" it
/// builds has to match `base`'s exact numeric type, which isn't known here).
#[derive(Debug, Clone)]
pub struct IncrementExpr {
    pub base: ExpressionNode,
}

/// `--base` -- see `IncrementExpr`.
#[derive(Debug, Clone)]
pub struct DecrementExpr {
    pub base: ExpressionNode,
}
