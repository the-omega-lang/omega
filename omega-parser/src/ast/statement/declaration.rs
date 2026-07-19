use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
    /// `true` only for a statement-position `mut ident: Type;` -- always
    /// `false` for a struct/enum field or an ordinary function parameter,
    /// since `mut` is never recognized in those positions at all
    /// (`parse_declaration_list` doesn't check for it). `self` is the one
    /// exception: `mut self` (by value) desugars to an immutable `self`
    /// parameter plus an implicit `mut self := self;` shadow -- see
    /// `FunctionDefinitionStmt::self_mode`/`SelfMode` and
    /// `omega_hir::lower::Lowerer::self_param`. See
    /// `omega_analyzer::context::VarBinding::mutable`.
    pub mutable: bool,
}
