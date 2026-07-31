use crate::ast::expression::ExpressionNode;

/// `comp base` -- evaluate `base` at compile time. Parsed at the same
/// `parse_unary` precedence tier as `reveal`/`Deref`/`AddressOf` (see
/// `parser::expression::parse_unary`), and like `reveal`, `base` isn't
/// restricted to place-shaped expressions -- `comp add(10, 20)` and `comp
/// MyThing { field = 1; }` are both legal. See `omega_hir::hir::HirExpr::Comp`'s
/// doc comment for how analysis handles this (an interpreter, not a second
/// type-checker -- `base` is analyzed completely ordinarily first).
#[derive(Debug, Clone)]
pub struct CompExpr {
    pub base: ExpressionNode,
}
