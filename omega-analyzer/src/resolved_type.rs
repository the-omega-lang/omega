use omega_hir::HirId;
use omega_parser::prelude::Ident;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFunctionType {
    pub params: Vec<(Ident, ResolvedType)>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
    pub is_member_function: bool,
}

/// A struct method's resolved type, plus the `HirId` of its declaring
/// `HirFunctionDef` -- unlike a field, a method has to be resolved back to a
/// callable symbol from *outside* the struct's own (already-popped)
/// analysis scope (see member-call resolution in `analysis.rs`), so its
/// declaration identity has to be recorded here, not just its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMethod {
    pub decl_id: HirId,
    pub fn_type: ResolvedFunctionType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStructType {
    pub fields: Vec<(Ident, ResolvedType)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
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
