use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;
use crate::ast::visibility::Visibility;

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
    /// `exposed`/`internal`/(default `Private`) -- an `extern` declaration
    /// is an ordinary top-level item like any other, so it gets the same
    /// treatment.
    pub visibility: Visibility,
}
