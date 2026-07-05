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

/// How a numeric resolved type behaves arithmetically: its signedness (or
/// float-ness) and bit width. Shared by analysis (to validate a number
/// literal's suffix, range-check its value, and type-check `BinaryOp`/
/// `Negate` operands) and codegen (to pick the right instruction --
/// `sdiv`/`udiv`, `ineg`/`fneg`, ...) -- computed once here rather than
/// re-pattern-matched on `ResolvedType` at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericKind {
    Signed(u32),
    Unsigned(u32),
    Float(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Void,
    Bool,
    /// A single Unicode scalar value, stored as a 4-byte codepoint -- the
    /// same representation Rust's `char` uses (large enough to hold any
    /// UTF-8-encoded character, decoded). This is *not* what a C string's
    /// bytes are typed as; that's `U8` (see `*u8`'s use for `puts`/`printf`
    /// in `examples/dev/main.omg`) -- a byte and a decoded character are
    /// different things once `char` stops being an alias for "one byte".
    Char,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Pointer(Box<ResolvedType>),
    Function(ResolvedFunctionType),
    /// An unsized run of `T`, only ever meaningful as a value type the way
    /// C's decayed array parameters are (see `argv : [*u8]` in
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

impl ResolvedType {
    /// `Some` for exactly the types a number literal can resolve to and
    /// `BinaryOp`/`Negate` can operate on -- notably excluding `Bool` and
    /// `Char`, neither of which supports arithmetic (matching e.g. Rust,
    /// where `char + char` and `bool + bool` are both errors).
    pub fn numeric_kind(&self) -> Option<NumericKind> {
        Some(match self {
            Self::I8 => NumericKind::Signed(8),
            Self::I16 => NumericKind::Signed(16),
            Self::I32 => NumericKind::Signed(32),
            Self::I64 => NumericKind::Signed(64),
            Self::U8 => NumericKind::Unsigned(8),
            Self::U16 => NumericKind::Unsigned(16),
            Self::U32 => NumericKind::Unsigned(32),
            Self::U64 => NumericKind::Unsigned(64),
            Self::F32 => NumericKind::Float(32),
            Self::F64 => NumericKind::Float(64),
            _ => return None,
        })
    }
}
