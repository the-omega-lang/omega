mod base_declaration;
pub mod extern_declaration;

use crate::syntax::statement::extern_declaration::ExternDeclaration;

pub enum Statement {
    ExternDeclaration(ExternDeclaration),
}
