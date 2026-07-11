use crate::ast::expression::ExpressionNode;
use crate::ast::r#type::Type;

/// `<Type>base` -- a plain expression-forming prefix operator, same
/// left-to-right shape as `NegateExpr`/`DerefExpr`/`AddressOfExpr`. Scoped
/// to numeric conversions (with real width/signedness-aware codegen) and
/// pointer/integer reinterpretation -- the parser doesn't restrict `target`
/// at all (it's the ordinary type grammar), but analysis rejects anything
/// that isn't castable (see `ResolvedType::cast_class`).
#[derive(Debug, Clone)]
pub struct CastExpr {
    pub target: Type,
    pub base: ExpressionNode,
}
