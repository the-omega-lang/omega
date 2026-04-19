use omega_parser::prelude::{FunctionType, Ident, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFunctionType {
    pub params: Vec<(Ident, ResolvedType)>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStructType {
    pub fields: Vec<(Ident, ResolvedType)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Void,
    Char,
    I32,
    Pointer(Box<ResolvedType>),
    Function(ResolvedFunctionType),
    Array(Box<ResolvedType>),
    Struct(ResolvedStructType),
}
