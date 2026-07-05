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
    /// An unsized run of `T`, only ever meaningful as a value type the way
    /// C's decayed array parameters are (see `argv : [*char]` in
    /// `examples/dev/main.omg`): a single thin pointer value, with no length
    /// carried alongside it. This is *not* what `*[T]` resolves to --
    /// see `Slice` below, and `Context::resolve_type`'s special case that
    /// produces it.
    Array(Box<ResolvedType>),
    /// `[T; N]` -- a sized, inline, contiguous run of exactly `N` `T`s.
    /// Unlike `Array`, this is a genuine value type: it's stored inline
    /// (locals, struct fields, ...) rather than referenced through a
    /// pointer, the same way a `Struct` is.
    SizedArray(Box<ResolvedType>, u32),
    /// `*[T]` -- a fat pointer: a data pointer plus a length, unlike
    /// `Pointer` which is always a single thin pointer value. Never written
    /// as `Pointer(Array(_))`; see `Context::resolve_type`.
    Slice(Box<ResolvedType>),
    Struct(ResolvedStructType),
}
