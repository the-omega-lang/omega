use crate::ast::expression::ExpressionNode;

/// A plain data tag, no parser-specific structure -- reused unchanged
/// through HIR, analysis, and codegen the same way `Ident`/`Type` already
/// are, rather than re-wrapped at each layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    /// `== != < <= > >=` -- unlike the arithmetic ops above, these always
    /// produce `bool` regardless of the (still-matching) operand type; see
    /// `Analyzer`'s `HirExpr::BinaryOp` arm.
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `& | ^ << >>` -- integer-only (rejects `Float`, same spirit as
    /// `Rem`'s `FloatRemainder`); see `Analyzer::analyze_binary_op`.
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        matches!(self, Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge)
    }

    /// The operator as the user wrote it -- for diagnostics ("cannot apply
    /// `%` to ..."), where the variant name (`Rem`) would just be noise.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::Shl => "<<",
            Self::Shr => ">>",
        }
    }
}

/// `left op right` -- a plain expression-forming operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser only knows this is
/// syntax, not whether/how it type-checks.
#[derive(Debug, Clone)]
pub struct BinaryOpExpr {
    pub left: ExpressionNode,
    pub op: BinaryOp,
    pub right: ExpressionNode,
}
