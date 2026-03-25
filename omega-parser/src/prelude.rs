pub use crate::syntax::SyntaxParser;
pub use crate::syntax::expression::{Expression, string::StringExpr};
pub use crate::syntax::identifier::Ident;
pub use crate::syntax::statement::{
    Statement, declaration::DeclarationStmt, extern_declaration::ExternDeclarationStmt,
};
pub use crate::syntax::r#type::Type;
