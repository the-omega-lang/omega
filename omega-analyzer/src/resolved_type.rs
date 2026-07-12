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

/// A union's fields and methods, shared behind `ResolvedType::Union`'s
/// `Rc<RefCell<_>>` for exactly the reasons `ResolvedStructType` is (see its
/// doc comment) -- same self-reference/placeholder-then-patch handling, same
/// nominal `PartialEq`/`Hash` below. The only real difference from a struct
/// is semantic (fields overlap in storage instead of being laid out
/// sequentially), which lives entirely in codegen/field-projection, not here.
#[derive(Debug)]
pub struct ResolvedUnionType {
    pub id: HirId,
    pub name: Ident,
    pub fields: Vec<(Ident, ResolvedType)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
}

impl PartialEq for ResolvedUnionType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedUnionType {}

impl Hash for ResolvedUnionType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// An omega-style enum's fully resolved shape, shared behind
/// `ResolvedType::Enum`'s `Rc<RefCell<_>>` for exactly the reasons
/// `ResolvedStructType` is (see its doc comment): a variant body may point
/// back at the enum itself (`next: *MyEnum`), so the placeholder is
/// registered before variants are resolved and patched in place.
///
/// Everything a *use site* needs is here -- construction sites in any
/// module read the tag/header constants straight out of this cell, so the
/// per-variant constants only ever get analyzed once, at the definition.
#[derive(Debug)]
pub struct ResolvedEnumType {
    pub id: HirId,
    pub name: Ident,
    /// Always an integer type -- `U16` for an implicit tag; whatever the
    /// header's leading `tag:` entry declared for an explicit one. Kept as
    /// a full `ResolvedType` (not a width/signedness pair) deliberately:
    /// the language intends to allow non-numeric tags eventually, and
    /// everything downstream already treats this as an opaque field type.
    pub tag_type: ResolvedType,
    /// The shared header fields, in declaration order -- *excluding* the
    /// tag, which is layout-wise field -1 (always first) and accessed via
    /// the dedicated `.tag` projection instead.
    pub header: Vec<(Ident, ResolvedType)>,
    pub variants: Vec<ResolvedEnumVariant>,
    /// Same shape and semantics as `ResolvedStructType::functions`.
    pub functions: Vec<(Ident, ResolvedMethod)>,
}

/// One resolved variant: its unique tag value, its per-variant header
/// constants (one per `ResolvedEnumType::header` entry, positionally), and
/// its own body fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnumVariant {
    pub name: Ident,
    /// Compile-time constant, unique across the enum -- what the
    /// uniqueness check compared, and what construction emits at offset 0.
    pub tag: crate::checked::NumberValue,
    /// One constant per header field, positionally.
    pub header_values: Vec<ConstValue>,
    /// The variant-specific body fields -- empty for a body-less variant.
    /// At runtime the enum's body region is a union of all variants'
    /// bodies; analysis only ever lets the statically-known variant's own
    /// fields be touched.
    pub fields: Vec<(Ident, ResolvedType)>,
}

impl ResolvedEnumType {
    /// The variant named `name`, with its index -- the shape both variant
    /// construction and body-field lookup want.
    pub fn variant(&self, name: &Ident) -> Option<(usize, &ResolvedEnumVariant)> {
        self.variants.iter().enumerate().find(|(_, v)| &v.name == name)
    }
}

/// Nominal identity, exactly like `ResolvedStructType`'s -- see its
/// `PartialEq`/`Hash` doc comments; the same self-reference reasoning
/// applies (a variant body may embed `*MyEnum`).
impl PartialEq for ResolvedEnumType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedEnumType {}

impl Hash for ResolvedEnumType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// A compile-time constant value -- what an enum variant's tag and header
/// values evaluate to at the definition, and what construction sites
/// re-emit. Covers exactly the primitive types a constant can currently be
/// written as; a header field whose type can't be represented here is
/// rejected at the enum's definition (see `AnalysisErrorKind::
/// EnumHeaderFieldUnsupportedType`), never at a use site.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Number(crate::checked::NumberValue),
    Bool(bool),
    Char(char),
    /// A `*u8` string constant -- the literal's decoded bytes.
    Str(String),
    /// A compile-time slice's elements (`&[...]`) -- no item type is
    /// carried here, exactly like `Str` doesn't carry its own type: it's
    /// always supplied externally by the enclosing `ResolvedType::Slice {
    /// item, .. }` at every call site (see `Analyzer::const_representable`,
    /// `Codegen::emit_const_value`). Codegen builds a separate rodata blob
    /// and stores a `[ptr, len]` fat pointer to it.
    Slice(Vec<ConstValue>),
    /// A fixed-length compile-time array's elements (a bare `[...]` against
    /// a `ResolvedType::SizedArray`-typed header field) -- unlike `Slice`,
    /// there's no indirection: codegen writes every element's leaves
    /// inline, back to back, directly into the enclosing storage (an enum's
    /// header region, or a nested array/slice element), exactly like an
    /// ordinary `SizedArray` value's own layout.
    Array(Vec<ConstValue>),
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

/// A castable type's shape, for `<Target>expr` (see `ResolvedType::cast_class`):
/// its bit width, and (for the int family) signedness -- exactly what's
/// needed to pick a `CastKind` between any two castable types, purely from
/// their widths/signedness, with no per-type-pair table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastClass {
    Int { width: u32, signed: bool },
    Float { width: u32 },
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
    /// `*T` (`mutable: false`) or `*mut T` (`mutable: true`) -- whether the
    /// pointee may be written through (`Analyzer::analyze_place`'s running
    /// mutability, overwritten by every `Deref` it processes). Immutable by
    /// default, like every binding (`VarBinding::mutable`). This is a
    /// *type*-level fact, unrelated to whether the pointer *itself* (as a
    /// binding) can be reassigned to point elsewhere.
    Pointer { pointee: Box<ResolvedType>, mutable: bool },
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
    /// `*[T]` (`mutable: false`) or `*mut [T]` (`mutable: true`) -- a fat
    /// pointer: a data pointer plus a length, unlike `Pointer` which is
    /// always a single thin pointer value. Never written as
    /// `Pointer(Array(_))`; see `Context::resolve_type`. `mutable` carries
    /// the same meaning `Pointer::mutable` does, for `slice[i] = value`.
    Slice { item: Box<ResolvedType>, mutable: bool },
    Struct(Rc<RefCell<ResolvedStructType>>),
    /// A C/Rust-style union value -- see `ResolvedUnionType`'s doc comment.
    Union(Rc<RefCell<ResolvedUnionType>>),
    /// An omega-style enum value. `variant` is the *statically known*
    /// variant, when there is one: `MyEnum::Second { ... }` produces a
    /// value of type `MyEnum::Second` (variant `Some(1)`), and only such a
    /// refined value may touch that variant's own body fields; a plain
    /// `MyEnum` (variant `None` -- what every written-down type annotation
    /// resolves to) only exposes the tag and the shared header. A refined
    /// value is usable anywhere the plain enum is expected -- the one
    /// implicit widening this type system has; see `ResolvedType::accepts`.
    Enum {
        cell: Rc<RefCell<ResolvedEnumType>>,
        variant: Option<usize>,
    },
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
            Self::Array(inner) => inner.hash(state),
            Self::Pointer { pointee, mutable } => {
                pointee.hash(state);
                mutable.hash(state);
            }
            Self::Slice { item, mutable } => {
                item.hash(state);
                mutable.hash(state);
            }
            Self::Function(fn_type) => fn_type.hash(state),
            Self::SizedArray(inner, size) => {
                inner.hash(state);
                size.hash(state);
            }
            Self::Struct(cell) => cell.borrow().hash(state),
            Self::Union(cell) => cell.borrow().hash(state),
            Self::Enum { cell, variant } => {
                cell.borrow().hash(state);
                variant.hash(state);
            }
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
            Self::Pointer { pointee, mutable: false } => write!(f, "*{pointee}"),
            Self::Pointer { pointee, mutable: true } => write!(f, "*mut {pointee}"),
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
            Self::Slice { item, mutable: false } => write!(f, "*[{item}]"),
            Self::Slice { item, mutable: true } => write!(f, "*mut [{item}]"),
            // Only the name, never the fields -- a struct may reference
            // itself, and its name is how source refers to it anyway.
            Self::Struct(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::Union(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            // A refined enum type shows its known variant (`MyEnum::Second`)
            // -- that's exactly how source spells the construction that
            // produced it, and the refinement is load-bearing in the
            // diagnostics that mention it (body-field access rules).
            Self::Enum { cell, variant } => {
                let e = cell.borrow();
                write!(f, "{}", e.name.as_ref())?;
                if let Some(index) = variant {
                    write!(f, "::{}", e.variants[*index].name.as_ref())?;
                }
                Ok(())
            }
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

    /// This type's shape for `<Target>expr` casting purposes -- `None` for
    /// anything a cast can't touch at all (structs/enums/unions/slices/
    /// `bool`/`char`/`void`/functions; see `AnalysisErrorKind::InvalidCast`).
    /// A pointer counts as an unsigned 64-bit int -- this compiler's
    /// existing single-target assumption (exactly matching `numeric_kind`'s
    /// own hardcoded 64-bit `isize`/`usize` above), and literally true at
    /// the IR level: `Codegen::pointer_type()` already returns the same
    /// Cranelift type an ordinary 64-bit integer would. This one case is
    /// what makes pointer<->pointer, pointer<->integer, and integer<->pointer
    /// casts all fall out of the *same* int-to-int width rules
    /// `Analyzer::resolve_cast_kind` applies, with no special-casing beyond
    /// it -- `bool`/`char` are deliberately left out (matching their
    /// existing exclusion from `numeric_kind`/arithmetic) rather than grown
    /// into a second special case here.
    pub fn cast_class(&self) -> Option<CastClass> {
        if let Some(kind) = self.numeric_kind() {
            return Some(match kind {
                NumericKind::Signed(width) => CastClass::Int { width, signed: true },
                NumericKind::Unsigned(width) => CastClass::Int { width, signed: false },
                NumericKind::Float(width) => CastClass::Float { width },
            });
        }
        match self {
            Self::Pointer { .. } => Some(CastClass::Int { width: 64, signed: false }),
            _ => None,
        }
    }

    /// The inclusive `[min, max]` domain of every representable value of
    /// this type, as `i128` (comfortably spans every integer type from
    /// `i8` to `u64`/`usize`, plus `bool`'s `{0,1}`) -- what a `match`'s
    /// interval-exhaustiveness check (`crate::exhaustiveness`) treats as
    /// "the whole domain" a numeric/`bool` match must cover. `None` for
    /// every other type: `match` support is deliberately scoped to enums,
    /// integers, and `bool` for now (see
    /// `AnalysisErrorKind::UnsupportedMatchScrutinee`) -- lifting that
    /// scope later (e.g. to `char`) only needs a new arm here.
    pub fn integer_domain(&self) -> Option<(i128, i128)> {
        Some(match self {
            Self::Bool => (0, 1),
            Self::I8 => (i8::MIN as i128, i8::MAX as i128),
            Self::I16 => (i16::MIN as i128, i16::MAX as i128),
            Self::I32 => (i32::MIN as i128, i32::MAX as i128),
            // `ISize` is hardcoded to 64 bits -- see `numeric_kind`'s doc
            // comment.
            Self::I64 | Self::ISize => (i64::MIN as i128, i64::MAX as i128),
            Self::U8 => (u8::MIN as i128, u8::MAX as i128),
            Self::U16 => (u16::MIN as i128, u16::MAX as i128),
            Self::U32 => (u32::MIN as i128, u32::MAX as i128),
            Self::U64 | Self::USize => (u64::MIN as i128, u64::MAX as i128),
            _ => return None,
        })
    }

    /// The same type with any statically-known enum-variant refinement
    /// erased (`MyEnum::Second` -> `MyEnum`) -- what inference positions
    /// that must stay variant-agnostic (an `if`'s unified branch type, an
    /// array literal's element type, a deduced generic argument) normalize
    /// to. Shallow on purpose: refinement only ever exists at the top level
    /// of a value's type (nothing written down in source can nest one).
    pub fn widened(&self) -> ResolvedType {
        match self {
            Self::Enum { cell, variant: Some(_) } => Self::Enum { cell: cell.clone(), variant: None },
            other => other.clone(),
        }
    }

    /// Whether a value of type `value` can be supplied where `self` is
    /// expected: exact equality, plus the one implicit widening this type
    /// system has -- a variant-refined enum value (`MyEnum::Second`) is
    /// usable as its plain enum (`MyEnum`). Never the reverse (a plain
    /// value's variant isn't known).
    ///
    /// This widening also applies through exactly one level of *immutable*
    /// pointer/slice indirection (`*MyEnum::Second` usable as `*MyEnum`) --
    /// sound specifically because of which pointers are ever allowed to
    /// carry a refined pointee in the first place: `&value` only keeps a
    /// refinement when it's a *permanent* fact about `value`'s own
    /// declared/inferred type (see `VarBinding::narrowed` and
    /// `Analyzer`'s `HirExpr::AddressOf` arm), and a permanently-refined
    /// binding can never be reassigned a different variant (this same
    /// `accepts` rule, applied at every assignment, already rejects that).
    ///
    /// Deliberately **not** extended to mutable pointers/slices at all --
    /// `*mut MyEnum::Second` never widens to `*mut MyEnum`, full stop, even
    /// though the exact same reasoning above would make it locally sound at
    /// this one call site. The reason is what happens *after*: a widened
    /// `*mut MyEnum` handed to unconstrained code could be used to write a
    /// *different* variant through it, silently invalidating whatever
    /// *other* binding/pointer still believes the underlying storage is
    /// `MyEnum::Second` (the original aliasing hole this whole mutability
    /// system exists to close). `&mut place`/`mut self`'s auto-ref close
    /// this at the *source* instead (see `Analyzer`'s `HirExpr::AddressOf`
    /// arm and `Context::widen_variable`): they always produce an already-
    /// widened mutable pointer and immediately widen the source binding's
    /// own tracked type too, so a refined mutable pointer only ever exists
    /// as a `match`-narrowed *view* of an already-mutable place, never as
    /// something `accepts` needs to reason about widening further.
    ///
    /// A mutable pointer/slice *does* freely coerce into an immutable one
    /// of the same (or widening-compatible) pointee, symmetric with a
    /// mutable binding being just as readable as an immutable one --
    /// captured below by `mutable: false` on `self`'s side alone.
    pub fn accepts(&self, value: &ResolvedType) -> bool {
        if self == value {
            return true;
        }
        match (self, value) {
            (Self::Enum { cell: expected, variant: None }, Self::Enum { cell: found, variant: Some(_) }) => {
                expected.borrow().id == found.borrow().id
            }
            (Self::Pointer { pointee: expected, mutable: false }, Self::Pointer { pointee: found, .. }) => {
                expected.accepts(found)
            }
            (Self::Slice { item: expected, mutable: false }, Self::Slice { item: found, .. }) => {
                expected.accepts(found)
            }
            _ => false,
        }
    }
}
