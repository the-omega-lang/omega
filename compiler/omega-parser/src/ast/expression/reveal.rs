use crate::ast::expression::ExpressionNode;

/// `reveal base` -- a visibility-bypass prefix, parsed at the same
/// `parse_unary` precedence tier as `Deref`/`AddressOf` (see
/// `parser::expression::parse_unary`). Unlike those, `base` isn't
/// restricted to place-shaped expressions: `reveal Struct { field = v }` and
/// `reveal foo()` are both legal, so this stays a generic wrapper rather
/// than folding into `HirPlace` -- see `omega_hir::hir::HirExpr::Reveal`'s
/// doc comment for how analysis handles that.
#[derive(Debug, Clone)]
pub struct RevealExpr {
    pub base: ExpressionNode,
}
