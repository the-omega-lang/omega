use crate::ast::expression::ExpressionNode;
use crate::ast::identifier::Ident;
use crate::ast::visibility::Visibility;

/// `ident := value;` -- "declare and assign", with `ident`'s type inferred
/// from `value`'s resolved type rather than written out explicitly like
/// `DeclarationStmt`. Shared by function-body statements (`Statement::
/// Walrus`) and top-level items (`Item::Walrus`, only legal `comp` -- see
/// `Item::Walrus`'s own doc comment); `visibility` is meaningful only for
/// the latter, left at its default (`Hidden`) for a local statement, same
/// "meaningless in most positions" treatment `DeclarationStmt::mutable`/
/// `visibility` document.
#[derive(Debug, Clone)]
pub struct WalrusStmt {
    pub ident: Ident,
    pub value: ExpressionNode,
    /// `true` only for `mut ident := value;`. See
    /// `omega_analyzer::context::VarBinding::mutable`.
    pub mutable: bool,
    /// `true` only for `comp ident := value;` -- `ident` carries no storage
    /// of its own; every reference to it is substituted with its already-
    /// evaluated value at compile time. Never `true` together with
    /// `mutable` in a checked tree (rejected during analysis, not parsing
    /// -- see `AnalysisErrorKind::MutCompBinding`). See
    /// `docs/19-compile-time-evaluation.md`.
    pub comp: bool,
    /// `exposed`/`internal`/(default `Hidden`) -- see this type's own doc
    /// comment for when this is meaningful.
    pub visibility: Visibility,
}
