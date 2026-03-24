use crate::syntax::identifier::Ident;

#[derive(Debug, Clone)]
pub enum Type {
    Named(Ident), // Identifier types. Example: i32, i64, char, ...
    Pointer(Box<Type>),
    Function {
        params: Vec<(Ident, Type)>,
        return_type: Box<Type>,
    },
}
