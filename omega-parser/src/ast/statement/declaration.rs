use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
    /// `true` only for a statement-position `mut ident: Type;` -- always
    /// `false` for a struct/enum field or a function parameter (including
    /// `self`, whose mutability is a *pointer* concern instead -- see
    /// `FunctionDefinitionStmt::self_mutable`), since `mut` is never
    /// recognized in those positions at all (`parse_declaration_list`
    /// doesn't check for it). See `omega_analyzer::context::VarBinding::mutable`.
    pub mutable: bool,
}
