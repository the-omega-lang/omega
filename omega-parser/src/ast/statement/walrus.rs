use crate::ast::expression::ExpressionNode;
use crate::ast::identifier::Ident;

/// `ident := value;` -- "declare and assign", with `ident`'s type inferred
/// from `value`'s resolved type rather than written out explicitly like
/// `DeclarationStmt`. Function-body statements only (not a `Item`):
/// a top-level `x := 5;` would hit the exact same "global data declarations
/// are not yet implemented" gap a top-level `x : i32;` already hits, so
/// there's no capability to gain from supporting it there yet.
#[derive(Debug, Clone)]
pub struct WalrusStmt {
    pub ident: Ident,
    pub value: ExpressionNode,
    /// `true` only for `mut ident := value;`. See
    /// `omega_analyzer::context::VarBinding::mutable`.
    pub mutable: bool,
}
