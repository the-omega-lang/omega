use crate::ast::expression::{ExpressionNode, codeblock::CodeblockExpr};
use crate::ast::range::RangeExpr;
use crate::diagnostics::Span;

/// `match scrutinee { pattern => body, ... } else { ... }` -- an exhaustive
/// switch, and (for an enum scrutinee) the proof mechanism that narrows a
/// matched place to a specific variant subtype inside the arm that proved
/// it (see `Pattern`'s doc comment). Deliberately shaped like `IfExpr`: a
/// genuine expression whose value is whichever arm's body ran, with
/// exhaustiveness (every arm's pattern set, or an explicit `else`, must
/// cover the scrutinee's whole domain) enforced by analysis, not here --
/// the parser only knows the shape.
#[derive(Debug, Clone)]
pub struct MatchExpr {
    pub scrutinee: ExpressionNode,
    pub arms: Vec<MatchArm>,
    pub else_branch: Option<CodeblockExpr>,
    pub span: Span,
}

/// `pattern => body` -- `body` is an ordinary expression (a `{ ... }`
/// codeblock is already `Expression::Codeblock`, so both a bare value and a
/// block fall out of the same `parse_expression` call; no separate "block
/// arm" shape is needed).
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: ExpressionNode,
    pub span: Span,
}

/// One arm's pattern. There is no destructuring/binding in this grammar
/// (deliberately, for now) -- a pattern only ever *proves* something about
/// the scrutinee, it never introduces new names.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// A literal (`100`, `'a'`, `true`) or an `Enum::Variant` path -- which
    /// one it is isn't decided here; analysis reads it against the
    /// scrutinee's own resolved type.
    Value(ExpressionNode),
    /// A range pattern (`RangeExpr`'s doc comment), matching a numeric
    /// scrutinee against an interval.
    Range(RangeExpr),
}
