use crate::ast::generics::GenericParam;
use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;
use crate::ast::statement::{declaration::DeclarationStmt, function_definition::FunctionDefinitionStmt};

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub ident: Ident,
    /// `<T, U, ...>` immediately after `ident` -- empty for an ordinary,
    /// non-generic struct. See `Type::Generic`'s doc comment for how these
    /// names are referenced at a use site.
    pub generics: Vec<GenericParam>,
    /// `: Spec1, Spec2, ...` right after the generics list -- the specs
    /// this struct implements. Each function they require must be provided
    /// either by this struct's own `functions` (an override) or by the
    /// spec's own default (used unmodified) -- see
    /// `Analyzer::signature_of_struct`'s implements-clause resolution.
    pub implements: Vec<Type>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}
