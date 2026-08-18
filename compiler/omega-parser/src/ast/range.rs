use crate::ast::expression::ExpressionNode;
use crate::diagnostics::Span;

#[derive(Debug, Clone)]
pub enum RangeEnd {
    Inclusive(ExpressionNode),
    Exclusive(ExpressionNode),
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

#[derive(Debug, Clone)]
pub struct RangeExpr {
    pub start: Option<ExpressionNode>,
    pub end: RangeEnd,
    pub span: Span,
}

impl RangeExpr {
    pub fn is_catch_all(&self) -> bool {
        self.start.is_none() && matches!(self.end, RangeEnd::Open)
    }

    pub fn inclusive(&self) -> bool {
        self.end.inclusive()
    }
}
