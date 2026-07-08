use omega_hir::HirId;
use omega_parser::prelude::Ident;
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// A struct's fields and methods, shared behind `ResolvedType::Struct`'s
/// `Rc<RefCell<_>>` so that a self-referencing field (`next: *Node`, the
/// classic linked-list shape) can hold a live handle to the very type still
/// being built: the placeholder is inserted (with empty `fields`/
/// `functions`) *before* fields are resolved, and patched in place once
/// they're known -- every clone taken in the meantime (e.g. a pointer field
/// that pointed back to it) observes the same, eventually-complete data,
/// rather than a stale structural snapshot copied by value. Comparing two
/// `ResolvedType::Struct`s (see `PartialEq` below) never has to walk into
/// `fields`/`functions` at all, so this also sidesteps the infinite regress
/// a *structural* comparison of a self-referential type would otherwise be.
#[derive(Debug)]
pub struct ResolvedStructType {
    pub id: HirId,
    pub name: Ident,
    pub fields: Vec<(Ident, ResolvedType)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
}

/// Nominal, not structural: two struct types are the same type iff they're
/// the same *declaration* (matching real language semantics -- two
/// unrelated structs that happen to share a field layout are still
/// different types), and, just as importantly, this never has to borrow
/// into (or recurse through) `fields`, which may reference this very struct
/// again -- comparing by id keeps that O(1) regardless.
impl PartialEq for ResolvedStructType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedStructType {}

/// Consistent with the identity-only `PartialEq` above -- hashing only
/// `id` (never `fields`/`functions`) is both correct (equal values must hash
/// equal, and equality here is id-only) and the only option that doesn't
/// recurse into a possibly self-referential struct's own fields.
impl Hash for ResolvedStructType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
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
    /// Pointer-sized signed integer. Hardcoded to 64 bits in `numeric_kind`
    /// below, matching this compiler's existing single-target reality (see
    /// its doc comment) -- it tracks the *target's* pointer width, not a
    /// fixed alias for `i64`, unlike `into_ir_type`'s mapping of this variant
    /// to `codegen.pointer_type()`, which genuinely is target-correct.
    ISize,
    U8,
    U16,
    U32,
    U64,
    /// Pointer-sized unsigned integer. See `ISize`'s doc comment.
    USize,
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
    Struct(Rc<RefCell<ResolvedStructType>>),
}

/// Can't `#[derive(Hash)]` -- `Rc<RefCell<ResolvedStructType>>` isn't
/// `Hash` (std deliberately omits it for `RefCell`, since mutating a key
/// after it's hashed into a map would silently break the map's invariants).
/// Mirrors the manual `PartialEq` derived transitively through `Struct`
/// above: hash the borrowed cell's `id` only, never its `fields`/
/// `functions`, both for consistency with that equality and to avoid
/// recursing into a possibly self-referential struct.
impl Hash for ResolvedType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Void
            | Self::Bool
            | Self::Char
            | Self::I8
            | Self::I16
            | Self::I32
            | Self::I64
            | Self::ISize
            | Self::U8
            | Self::U16
            | Self::U32
            | Self::U64
            | Self::USize
            | Self::F32
            | Self::F64 => {}
            Self::Pointer(inner) | Self::Array(inner) | Self::Slice(inner) => inner.hash(state),
            Self::Function(fn_type) => fn_type.hash(state),
            Self::SizedArray(inner, size) => {
                inner.hash(state);
                size.hash(state);
            }
            Self::Struct(cell) => cell.borrow().hash(state),
        }
    }
}

/// Renders the type exactly as a user would write it in Omega source
/// (`*u8`, `[i32; 3]`, `*[u8]`, `(s: *u8, ...) => i32`, a struct's bare
/// name) -- this is what every diagnostic shows, so it must read as the
/// language's own syntax, never as Rust debug output.
impl std::fmt::Display for ResolvedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Void => write!(f, "void"),
            Self::Bool => write!(f, "bool"),
            Self::Char => write!(f, "char"),
            Self::I8 => write!(f, "i8"),
            Self::I16 => write!(f, "i16"),
            Self::I32 => write!(f, "i32"),
            Self::I64 => write!(f, "i64"),
            Self::ISize => write!(f, "isize"),
            Self::U8 => write!(f, "u8"),
            Self::U16 => write!(f, "u16"),
            Self::U32 => write!(f, "u32"),
            Self::U64 => write!(f, "u64"),
            Self::USize => write!(f, "usize"),
            Self::F32 => write!(f, "f32"),
            Self::F64 => write!(f, "f64"),
            Self::Pointer(inner) => write!(f, "*{inner}"),
            Self::Function(fn_type) => {
                write!(f, "(")?;
                for (i, (name, param)) in fn_type.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if name.as_ref().is_empty() {
                        write!(f, "{param}")?;
                    } else {
                        write!(f, "{}: {param}", name.as_ref())?;
                    }
                }
                if fn_type.is_variadic {
                    if !fn_type.params.is_empty() {
                        write!(f, ", ")?;
                    }
                    write!(f, "...")?;
                }
                write!(f, ") => {}", fn_type.return_type)
            }
            Self::Array(inner) => write!(f, "[{inner}]"),
            Self::SizedArray(inner, size) => write!(f, "[{inner}; {size}]"),
            Self::Slice(inner) => write!(f, "*[{inner}]"),
            // Only the name, never the fields -- a struct may reference
            // itself, and its name is how source refers to it anyway.
            Self::Struct(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
        }
    }
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
            // Hardcoded to 64 bits -- see `ISize`/`USize`'s doc comments.
            Self::ISize => NumericKind::Signed(64),
            Self::U8 => NumericKind::Unsigned(8),
            Self::U16 => NumericKind::Unsigned(16),
            Self::U32 => NumericKind::Unsigned(32),
            Self::U64 => NumericKind::Unsigned(64),
            Self::USize => NumericKind::Unsigned(64),
            Self::F32 => NumericKind::Float(32),
            Self::F64 => NumericKind::Float(64),
            _ => return None,
        })
    }
}
