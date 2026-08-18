use omega_hir::{HirBlock, HirId, HirParam};
use omega_parser::prelude::{Ident, SelfMode, Span, Type, Visibility};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedFunctionType {
    pub params: Vec<(Ident, ResolvedType)>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
    /// `None` for an ordinary function; `Some` for a member function,
    /// carrying exactly how it receives `self` -- see `SelfMode`. Never
    /// reverse-engineered from `params[0]`'s resolved type shape.
    pub self_mode: Option<SelfMode>,
}

/// A struct method's resolved type, plus the `HirId` of its declaring
/// `HirFunctionDef` -- unlike a field, a method has to be resolved back to a
/// callable symbol from outside the struct's own already-popped analysis
/// scope, so its declaration identity is recorded here too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMethod {
    pub decl_id: HirId,
    pub fn_type: ResolvedFunctionType,
    /// `exposed`/`internal`/(default `Hidden`), resolved once here at
    /// signature time -- enforced in `Analyzer::check_visibility`'s
    /// method-call hook (`resolve_callee`).
    pub visibility: Visibility,
    /// This method's own resolved `@inline`/`@mangling`/`@suppress` --
    /// resolved once at signature time (never re-resolved at body-check
    /// time) so it's already known even for an extern-owned method whose
    /// body this compilation never checks.
    pub annotations: crate::annotations::ResolvedAnnotations,
    pub source: Option<ConformanceSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceSource {
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConformance {
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
}

pub type ResolvedBound = (
    ResolvedType,
    Rc<RefCell<ResolvedSpecType>>,
    Vec<ResolvedType>,
);

/// A struct's fields and methods, shared behind `ResolvedType::Struct`'s
/// `Rc<RefCell<_>>` so a self-referencing field (`next: *Node`) can hold a
/// live handle to the very type still being built: a placeholder cell is
/// inserted before fields are resolved, then patched in place once known,
/// so every clone taken meanwhile observes the same eventually-complete
/// data. `PartialEq` below never walks into `fields`/`functions`, so this
/// also sidesteps the infinite regress a structural comparison would be.
#[derive(Debug)]
pub struct ResolvedStructType {
    pub id: HirId,
    pub name: Ident,
    /// The absolute path of the module this struct is declared in --
    /// a reference to this type from elsewhere only ever sees this cell,
    /// never the declaration site, and mangling a symbol needs a full path.
    pub module_path: Vec<Ident>,
    /// The concrete generic arguments this cell was instantiated with --
    /// empty for a non-generic struct. Lets a reference to this type from
    /// elsewhere still be mangled with its generic arguments intact.
    pub type_args: Vec<ResolvedType>,
    /// `(name, type, visibility)` per field, in declaration order -- the
    /// third element is enforced by `Analyzer::check_visibility`'s
    /// field-access hook (`resolve_field_projection`).
    pub fields: Vec<(Ident, ResolvedType, Visibility)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    /// `@layout(...)`'s resolved `pack`/`align` -- `{1, 1}` unless
    /// overridden. Read by `omega_codegen`'s layout functions.
    pub layout: crate::annotations::Layout,
    /// `@suppress(...)`'s warning names -- resolved once here rather than
    /// re-resolved every time a method body is checked, so
    /// `check_struct_body` doesn't risk re-emitting the same annotation
    /// errors twice.
    pub suppress: Vec<Ident>,
    /// `true` for a `marker` declaration. Exempts this cell from the "a
    /// struct must have at least one sized field" check -- everything else
    /// already works unmodified for a zero-field struct, which is why
    /// `marker` reuses this type wholesale instead of a separate
    /// `ResolvedType` variant.
    pub is_marker: bool,
}

/// Nominal, not structural: two struct types are equal iff they're the
/// same declaration. Never borrows into `fields` (which may reference this
/// very struct again), keeping comparison O(1).
impl PartialEq for ResolvedStructType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedStructType {}

/// Consistent with the identity-only `PartialEq` above -- hashes only
/// `id`, never `fields`/`functions`, avoiding recursion into a possibly
/// self-referential struct.
impl Hash for ResolvedStructType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// A union's fields and methods, shared behind `ResolvedType::Union`'s
/// `Rc<RefCell<_>>` for the same reasons `ResolvedStructType` is. The only
/// difference from a struct is semantic (fields overlap in storage rather
/// than laying out sequentially), which lives in codegen/field-projection,
/// not here.
#[derive(Debug)]
pub struct ResolvedUnionType {
    pub id: HirId,
    pub name: Ident,
    /// See `ResolvedStructType::module_path`'s doc comment.
    pub module_path: Vec<Ident>,
    /// See `ResolvedStructType::type_args`'s doc comment.
    pub type_args: Vec<ResolvedType>,
    /// See `ResolvedStructType::fields`'s doc comment.
    pub fields: Vec<(Ident, ResolvedType, Visibility)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    /// See `ResolvedStructType::suppress`'s doc comment. Unions don't
    /// support `@layout` yet -- only `@suppress` applies here.
    pub suppress: Vec<Ident>,
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
/// `ResolvedType::Enum`'s `Rc<RefCell<_>>` for the same reasons
/// `ResolvedStructType` is: a variant body may point back at the enum
/// itself (`next: *MyEnum`), so the placeholder is registered before
/// variants are resolved and patched in place. Construction sites in any
/// module read the tag/header constants straight out of this cell, so
/// per-variant constants are analyzed once, at the definition.
#[derive(Debug)]
pub struct ResolvedEnumType {
    pub id: HirId,
    pub name: Ident,
    /// See `ResolvedStructType::module_path`'s doc comment.
    pub module_path: Vec<Ident>,
    /// See `ResolvedStructType::type_args`'s doc comment.
    pub type_args: Vec<ResolvedType>,
    /// Always an integer type -- `U16` for an implicit tag; whatever the
    /// header's leading `tag:` entry declared for an explicit one. Kept as
    /// a full `ResolvedType` rather than a width/signedness pair since the
    /// language intends to allow non-numeric tags eventually.
    pub tag_type: ResolvedType,
    /// The shared header fields, in declaration order -- excluding the
    /// tag, which is layout-wise field -1 and accessed via the dedicated
    /// `.tag` projection instead.
    pub header: Vec<(Ident, ResolvedType, Visibility)>,
    /// The shared dynamic fields, in declaration order -- present on every
    /// variant like `header`, laid out right after it, but runtime-valued:
    /// every construction site supplies them, and they're freely
    /// assignable afterward like a variant's own body field. Unlike
    /// `header`, there is no per-variant constant list here.
    pub dynamic_fields: Vec<(Ident, ResolvedType, Visibility)>,
    pub variants: Vec<ResolvedEnumVariant>,
    /// Same shape and semantics as `ResolvedStructType::functions`.
    pub functions: Vec<(Ident, ResolvedMethod)>,
    /// Same shape and semantics as `ResolvedStructType::layout`, applied
    /// to the enum's own aggregate `[tag][header][dynamic][payload]`
    /// layout as a whole.
    pub layout: crate::annotations::Layout,
    /// See `ResolvedStructType::suppress`.
    pub suppress: Vec<Ident>,
}

/// One resolved variant: its unique tag value, its per-variant header
/// constants (one per `ResolvedEnumType::header` entry, positionally), and
/// its own body fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnumVariant {
    pub name: Ident,
    /// Compile-time constant, unique across the enum -- what construction
    /// emits at offset 0.
    pub tag: crate::checked::NumberValue,
    /// One constant per header field, positionally.
    pub header_values: Vec<ConstValue>,
    /// The variant-specific body fields -- empty for a body-less variant.
    /// At runtime the enum's body region is a union of all variants'
    /// bodies; analysis only lets the statically-known variant's own
    /// fields be touched.
    pub fields: Vec<(Ident, ResolvedType, Visibility)>,
}

impl ResolvedEnumType {
    /// The variant named `name`, with its index -- the shape both variant
    /// construction and body-field lookup want.
    pub fn variant(&self, name: &Ident) -> Option<(usize, &ResolvedEnumVariant)> {
        self.variants
            .iter()
            .enumerate()
            .find(|(_, v)| &v.name == name)
    }
}

/// Nominal identity, exactly like `ResolvedStructType`'s -- the same
/// self-reference reasoning applies (a variant body may embed `*MyEnum`).
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

/// A `spec`'s fully resolved shape, shared behind `ResolvedType::Spec`'s
/// `Rc<RefCell<_>>` for the same nominal-identity reasons
/// `ResolvedStructType` is -- though a spec is never self-referential, so
/// the placeholder-then-patch dance doesn't apply; it's still behind a
/// cell purely so every reference to "this spec" shares one identity to
/// compare/hash against.
///
/// `dependencies` are already resolved (not raw) -- an alias's member list
/// needs no `Self`/generic substitution to know *which* specs it names, so
/// resolving them eagerly here makes alias-cycle detection fall out of the
/// ordinary `ensure_item` `InProgress` machinery for free, with no
/// spec-specific cycle guard needed. Only an alias populates it: spec
/// provisioning (`spec X : A, B`) no longer exists, so an ordinary
/// declaration's member list is always empty.
#[derive(Debug)]
pub struct ResolvedSpecType {
    pub id: HirId,
    pub name: Ident,
    /// `exposed`/`internal`/(default `Hidden`) -- the spec's own
    /// visibility. Kept here too because every one of this spec's
    /// functions inherits it: an implementor's method satisfying one must
    /// be at least this permissive, checked during conform checking.
    pub visibility: Visibility,
    pub generics: Vec<Ident>,
    /// See `ResolvedStructType::module_path`.
    pub module_path: Vec<Ident>,
    /// See `ResolvedStructType::type_args` -- the concrete arguments
    /// `generics` was substituted with, empty for a non-generic spec.
    pub type_args: Vec<ResolvedType>,
    /// Whether this spec can be used as a dynamic-dispatch trait object
    /// (`spec *Self`) -- `false` the instant any of `functions` declares a
    /// `spec T` (static-dispatch) return type, directly or transitively
    /// through an alias member: a vtable slot can't point at "whichever
    /// concrete type each implementor happens to use" (the same reason
    /// Rust's `IntoIterator` isn't object-safe). Computed once, eagerly,
    /// where `functions`/`dependencies` are first resolved. Checked
    /// wherever a `Type::SpecObject` actually resolves into a real
    /// `ResolvedType::SpecObject`.
    pub is_object_safe: bool,
    /// Whether this spec is a pure alias (`spec X = A + B;`) rather than
    /// an ordinary declaration. An alias is never itself conformable -- it
    /// names a conjunction of other specs, satisfied by conforming each
    /// member separately.
    pub is_alias: bool,
    /// The alias form's members, each resolved eagerly, paired with its
    /// **raw**, unresolved type arguments -- deliberately not
    /// `Vec<ResolvedType>`: resolving them here would need this spec's own
    /// generics already bound to something concrete, which they never are
    /// at this point (`spec Foo<T> = Bar<T> + Baz;` -- `T` isn't concrete
    /// until a real implementor is known). Resolved lazily instead, in
    /// `Analyzer::flatten_spec_into`, once `Self` and this spec's own
    /// generics *are* bound. Always empty for a non-alias declaration.
    pub dependencies: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)>,
    pub functions: Vec<(Ident, RawSpecFunctionSig)>,
    /// This spec's own resolved `@suppress` list -- see
    /// `ResolvedStructType::suppress`.
    pub suppress: Vec<Ident>,
}

impl PartialEq for ResolvedSpecType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedSpecType {}

impl Hash for ResolvedSpecType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// One function member of a spec, kept **raw** (unresolved `Type`s, an
/// unlowered `HirBlock` default body) -- mirroring `GenericSignature`'s
/// precedent: a `Self`-referencing (or spec-generic-referencing) type
/// can't be resolved to a concrete `ResolvedType` until a concrete
/// implementor is known, so resolution is deferred to that point
/// (`Analyzer::signature_of_struct`'s conformance handling) rather than
/// attempted at the spec's own definition.
#[derive(Debug, Clone)]
pub struct RawSpecFunctionSig {
    pub decl_id: HirId,
    pub name: Ident,
    pub span: Span,
    /// Carried alongside `span` so a queued default-method instantiation
    /// can rebuild a `HirFunctionDef` with the spec function's real
    /// signature spans rather than widening every diagnostic to the whole
    /// declaration.
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    /// See `ResolvedFunctionType::self_mode`. Always `Pointer`/`MutPointer`
    /// in practice -- by-value self is rejected at spec signature
    /// resolution.
    pub self_mode: Option<SelfMode>,
    /// Raw `HirParam`s rather than a plain `(Ident, Type)` list -- lets a
    /// queued default-method instantiation reconstruct a real
    /// `HirFunctionDef` later and reuse `check_function_body` wholesale
    /// instead of duplicating its param-binding logic.
    pub params: Vec<HirParam>,
    pub is_variadic: bool,
    pub return_type: Type,
    /// `None` for a required function (every implementor must provide its
    /// own matching method); `Some` for a default, used as-is unless a
    /// concrete implementor overrides it with its own same-named,
    /// same-signature method.
    pub default_body: Option<HirBlock>,
}

/// One first-class gap function, fully resolved at the gap declaration.
#[derive(Debug, Clone)]
pub struct GapFunction {
    pub decl_id: HirId,
    pub span: Span,
    pub fn_type: ResolvedFunctionType,
}

/// A first-class gap: a global namespace of fully-resolved, self-less
/// function signatures. It is deliberately not a `ResolvedType` -- it can
/// neither occur in a type annotation nor participate in spec conformance.
#[derive(Debug, Clone)]
pub struct ResolvedGap {
    pub id: HirId,
    pub name: Ident,
    pub module_path: Vec<Ident>,
    pub span: Span,
    pub functions: Vec<(Ident, GapFunction)>,
}

/// A compile-time constant value -- what an enum variant's tag and header
/// values evaluate to at the definition, and what construction sites
/// re-emit. A header field whose type can't be represented here is
/// rejected at the enum's definition, never at a use site.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Number(crate::checked::NumberValue),
    Bool(bool),
    Char(char),
    /// A `*str` string constant -- the literal's decoded UTF-8 bytes.
    Str(String),
    /// A compile-time slice's elements (`&[...]`) -- no item type is
    /// carried here, exactly like `Str`: it's always supplied externally
    /// by the enclosing `ResolvedType::Slice { item, .. }`. Codegen builds
    /// a separate rodata blob and stores a `[ptr, len]` fat pointer to it.
    Slice(Vec<ConstValue>),
    /// A fixed-length compile-time array's elements -- unlike `Slice`,
    /// no indirection: codegen writes every element's leaves inline, back
    /// to back, directly into the enclosing storage.
    Array(Vec<ConstValue>),
    /// A whole struct value, built by `comp` evaluation -- fields in
    /// declared order, so codegen can write leaves in list order with no
    /// name lookup, like `Array`'s elements.
    Struct(Vec<ConstValue>),
    /// A whole enum value, built by `comp` evaluation. `tag` and `header`
    /// are embedded directly rather than re-derived from the enum's
    /// shared `ResolvedEnumType` cell, since a `ConstValue` carries no
    /// reference back to its own `ResolvedType`. `dynamic_fields`/`fields`
    /// split `CheckedEnumConstruct::fields`'s combined list back into its
    /// two real regions.
    Enum {
        variant_index: usize,
        tag: crate::checked::NumberValue,
        header: Vec<ConstValue>,
        dynamic_fields: Vec<ConstValue>,
        fields: Vec<ConstValue>,
    },
    /// A whole union value, built by `comp` evaluation -- mirrors
    /// `CheckedUnionConstruct`: exactly one field written, at `field_index`.
    Union {
        field_index: usize,
        value: Box<ConstValue>,
    },
    /// The address of another piece of `comp`-evaluated data (`&<place>`
    /// where `<place>` itself evaluated cleanly) -- generalizes what `Str`/
    /// `Slice` do ad hoc into one explicit indirection, so a struct field
    /// can point at e.g. a sibling `comp`-computed buffer. Never a pointer
    /// into runtime memory -- the interpreter only produces one of these
    /// by evaluating a place it fully evaluated itself.
    Ref(Box<ConstValue>),
}

/// How a numeric resolved type behaves arithmetically: its signedness (or
/// float-ness) and bit width. Shared by analysis (literal suffixes, range
/// checks, `BinaryOp`/`Negate` type-checking) and codegen (picking
/// `sdiv`/`udiv`, `ineg`/`fneg`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericKind {
    Signed(u32),
    Unsigned(u32),
    Float(u32),
}

/// A castable type's shape, for `<Target>expr` (see
/// `ResolvedType::cast_class`): bit width and, for the int family,
/// signedness -- enough to pick a `CastKind` between any two castable
/// types purely from width/signedness, with no per-type-pair table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastClass {
    Int { width: u32, signed: bool },
    Float { width: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Void,
    /// `never` -- a function's own declared return type meaning "this
    /// function does not return" (`exit(code: i32) => never`). Legal only
    /// in that one position; `Context::resolve_type` rejects it everywhere
    /// else a type gets resolved for storage. Deliberately not threaded
    /// through `accepts`/general expression-type inference: it only needs
    /// to exist so a call's looked-up return type can *be* `Never` at the
    /// point that matters -- `Analyzer::expr_diverges` recognizing such a
    /// call as diverging, after which the existing `None`-means-diverges
    /// mechanism (`Analyzer::block_type`) does the rest.
    Never,
    Bool,
    /// A single Unicode scalar value, stored as a 4-byte codepoint, like
    /// Rust's `char`. Not what a C string's bytes are typed as -- that's
    /// `U8`; a byte and a decoded character are different things.
    Char,
    I8,
    I16,
    I32,
    I64,
    /// Pointer-sized signed integer -- tracks the target's pointer width
    /// (`Target::pointer_bits`), never a fixed alias for `i64`: genuinely
    /// 32 bits on a 32-bit target.
    ISize,
    U8,
    U16,
    U32,
    U64,
    /// Pointer-sized unsigned integer. See `ISize`.
    USize,
    F32,
    F64,
    /// `*T` (`mutable: false`) or `*mut T` (`mutable: true`) -- whether
    /// the pointee may be written through. A type-level fact, unrelated
    /// to whether the pointer itself (as a binding) can be reassigned to
    /// point elsewhere.
    Pointer {
        pointee: Box<ResolvedType>,
        mutable: bool,
    },
    Function(ResolvedFunctionType),
    /// `*[?]T` (`mutable: false`) or `*mut [?]T` (`mutable: true`) -- an
    /// unsized run of `T`: a thin pointer value with array-like properties
    /// (indexing, slicing), the same C-decayed-array-parameter shape
    /// `argv : *[]*u8` uses. Mutability is a type-level fact exactly like
    /// `Pointer`'s. Not what `*[]T` resolves to -- see `Slice` below.
    Array(Box<ResolvedType>, bool),
    /// `[N]T` -- a sized, inline, contiguous run of exactly `N` `T`s.
    /// Unlike `Array`, this is a genuine value type: it's stored inline
    /// (locals, struct fields, ...) rather than referenced through a
    /// pointer, the same way a `Struct` is.
    SizedArray(Box<ResolvedType>, u32),
    /// `*[]T` (`mutable: false`) or `*mut []T` (`mutable: true`) -- a fat
    /// pointer: a data pointer plus a length, unlike `Pointer`'s single
    /// thin pointer. Never written as `Pointer(Array(_))`. `mutable`
    /// carries the same meaning `Pointer::mutable` does.
    Slice {
        item: Box<ResolvedType>,
        mutable: bool,
    },
    /// `*str` (`mutable: false`) or `*mut str` (`mutable: true`) -- a
    /// UTF-8 string slice: at runtime the same fat-pointer shape as
    /// `Slice { item: U8, .. }` (no null terminator), but a genuinely
    /// distinct nominal type with no implicit coercion to/from
    /// `Slice`/`Pointer`. `str` alone names nothing on its own -- it only
    /// resolves to this variant via the raw-pointee special case in
    /// `Context::resolve_type`'s `Type::Pointer` arm.
    Str {
        mutable: bool,
    },
    Struct(Rc<RefCell<ResolvedStructType>>),
    /// A C/Rust-style union value -- see `ResolvedUnionType`'s doc comment.
    Union(Rc<RefCell<ResolvedUnionType>>),
    /// An omega-style enum value. `variant` is the statically known
    /// variant, when there is one: `MyEnum::Second { ... }` produces a
    /// value of type `MyEnum::Second`, and only such a refined value may
    /// touch that variant's own body fields; a plain `MyEnum` (variant
    /// `None`) only exposes the tag and shared header. See `accepts` for
    /// the implicit widening from refined to plain.
    Enum {
        cell: Rc<RefCell<ResolvedEnumType>>,
        variant: Option<usize>,
    },
    /// A reference to a spec *definition* -- what a conform declaration,
    /// a generic bound (`T: Animal`), or a spec-object type's pointee
    /// resolves the name `Animal` to. Never itself the type of a runtime
    /// value -- a `spec *Animal` value's type is `SpecObject` below.
    Spec(Rc<RefCell<ResolvedSpecType>>),
    /// `spec *Animal` (`mutable: false`) or `spec *mut Animal` (`mutable:
    /// true`) -- a dynamic-dispatch trait-object value: a fat pointer (a
    /// data pointer plus a compiler-generated vtable pointer), like
    /// `Slice`'s `[data_ptr, len]` is a fat pointer of a different kind.
    /// The concrete pointee type is erased -- only that it implements
    /// `spec` (with these `type_args`, for a generic spec) is known.
    SpecObject {
        spec: Rc<RefCell<ResolvedSpecType>>,
        type_args: Vec<ResolvedType>,
        mutable: bool,
    },
}

/// Can't `#[derive(Hash)]` -- `Rc<RefCell<ResolvedStructType>>` isn't
/// `Hash` (std omits it for `RefCell`, since mutating a key after it's
/// hashed would break the map's invariants). Hashes the borrowed cell's
/// `id` only, consistent with the manual `PartialEq`.
impl Hash for ResolvedType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Void
            | Self::Never
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
            Self::Array(inner, mutable) => {
                inner.hash(state);
                mutable.hash(state);
            }
            Self::Pointer { pointee, mutable } => {
                pointee.hash(state);
                mutable.hash(state);
            }
            Self::Slice { item, mutable } => {
                item.hash(state);
                mutable.hash(state);
            }
            Self::Str { mutable } => mutable.hash(state),
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
            Self::Spec(cell) => cell.borrow().hash(state),
            Self::SpecObject {
                spec,
                type_args,
                mutable,
            } => {
                spec.borrow().hash(state);
                type_args.hash(state);
                mutable.hash(state);
            }
        }
    }
}

/// Renders the type exactly as a user would write it in Omega source --
/// what every diagnostic shows, so it must read as the language's own
/// syntax, never as Rust debug output.
impl std::fmt::Display for ResolvedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Void => write!(f, "void"),
            Self::Never => write!(f, "never"),
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
            Self::Pointer {
                pointee,
                mutable: false,
            } => write!(f, "*{pointee}"),
            Self::Pointer {
                pointee,
                mutable: true,
            } => write!(f, "*mut {pointee}"),
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
            Self::Array(inner, false) => write!(f, "*[?]{inner}"),
            Self::Array(inner, true) => write!(f, "*mut [?]{inner}"),
            Self::SizedArray(inner, size) => write!(f, "[{size}]{inner}"),
            Self::Slice {
                item,
                mutable: false,
            } => write!(f, "*[]{item}"),
            Self::Slice {
                item,
                mutable: true,
            } => write!(f, "*mut []{item}"),
            Self::Str { mutable: false } => write!(f, "*str"),
            Self::Str { mutable: true } => write!(f, "*mut str"),
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
            Self::Spec(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::SpecObject {
                spec,
                type_args,
                mutable,
            } => {
                write!(f, "spec *{}", if *mutable { "mut " } else { "" })?;
                write!(f, "{}", spec.borrow().name.as_ref())?;
                if !type_args.is_empty() {
                    write!(f, "<")?;
                    for (i, arg) in type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
        }
    }
}

impl ResolvedType {
    /// `Some` for exactly the types a number literal can resolve to, and
    /// the only types `BinaryOp`/`Negate`/`BitNot` operate on directly.
    /// `Bool`/`Char`/pointers still get arithmetic and bitwise ops by
    /// first coercing to one of these (see `arithmetic_repr`).
    ///
    /// `pointer_bits` is the target's pointer width -- `ISize`/`USize`
    /// track it, so their classification depends on it.
    pub fn numeric_kind(&self, pointer_bits: u32) -> Option<NumericKind> {
        Some(match self {
            Self::I8 => NumericKind::Signed(8),
            Self::I16 => NumericKind::Signed(16),
            Self::I32 => NumericKind::Signed(32),
            Self::I64 => NumericKind::Signed(64),
            Self::ISize => NumericKind::Signed(pointer_bits),
            Self::U8 => NumericKind::Unsigned(8),
            Self::U16 => NumericKind::Unsigned(16),
            Self::U32 => NumericKind::Unsigned(32),
            Self::U64 => NumericKind::Unsigned(64),
            Self::USize => NumericKind::Unsigned(pointer_bits),
            Self::F32 => NumericKind::Float(32),
            Self::F64 => NumericKind::Float(64),
            _ => return None,
        })
    }

    /// The numeric type a non-numeric operand implicitly coerces to for
    /// an arithmetic or bitwise op -- `None` for anything with no such
    /// stand-in, including `Char` and `Bool` (handled natively instead,
    /// see `Analyzer::analyze_binary_op`) and non-arithmetic types.
    ///
    /// The chosen representative always matches the exact scalar
    /// `layout::Leaf` codegen stores the type as, so the coercion is
    /// always a same-width `CastKind::Reinterpret`, free at runtime.
    pub fn arithmetic_repr(&self) -> Option<ResolvedType> {
        match self {
            Self::Pointer { .. } => Some(ResolvedType::USize),
            _ => None,
        }
    }

    /// This type's byte size, for a `sizeof<...>` used *inside* an
    /// annotation argument (`@layout(pack = sizeof<usize>)`) -- scoped to
    /// primitives only (`None` for structs/enums/unions/arrays/slices/
    /// functions/spec objects), since a primitive's size needs no real
    /// backend, only the target's pointer width, so `@layout`'s arguments
    /// can be resolved eagerly in the analyzer. `sizeof<Type>` used as an
    /// ordinary expression is not scoped this way -- it supports any type,
    /// computed in codegen via `total_bytes`.
    pub fn primitive_byte_size(&self, pointer_bytes: u32) -> Option<u32> {
        match self {
            Self::Bool => Some(1),
            Self::Char => Some(4),
            Self::Pointer { .. } => Some(pointer_bytes),
            _ => self.numeric_kind(pointer_bytes * 8).map(|kind| {
                let width = match kind {
                    NumericKind::Signed(w) | NumericKind::Unsigned(w) | NumericKind::Float(w) => w,
                };
                width / 8
            }),
        }
    }

    /// This type's shape for `<Target>expr` casting purposes -- `None`
    /// for anything a cast can't touch at all (structs/enums/unions/
    /// slices/`void`/functions). A pointer counts as an unsigned integer
    /// of the target's pointer width, literally true at the IR level (a
    /// pointer leaf and a `usize` leaf are the same width by
    /// construction), which is what makes pointer<->pointer,
    /// pointer<->integer, and integer<->pointer casts all fall out of the
    /// same int-to-int width rules with no special-casing.
    ///
    /// `Char`/`Bool` get a class the same way, but **only ever as the
    /// source** of a cast: this has no notion of direction, so
    /// `Analyzer::analyze_cast` gates the into-`Char`/`Bool` asymmetry
    /// explicitly (see `allows_cast_into`), since not every `u32` is a
    /// valid codepoint and there's no implicit "nonzero is true".
    /// `pointer_bits` is the target's pointer width -- `ISize`/`USize`
    /// classify at it.
    pub fn cast_class(&self, pointer_bits: u32) -> Option<CastClass> {
        if let Some(kind) = self.numeric_kind(pointer_bits) {
            return Some(match kind {
                NumericKind::Signed(width) => CastClass::Int {
                    width,
                    signed: true,
                },
                NumericKind::Unsigned(width) => CastClass::Int {
                    width,
                    signed: false,
                },
                NumericKind::Float(width) => CastClass::Float { width },
            });
        }
        match self {
            Self::Pointer { .. } => Some(CastClass::Int {
                width: pointer_bits,
                signed: false,
            }),
            Self::Char => Some(CastClass::Int {
                width: 32,
                signed: false,
            }),
            Self::Bool => Some(CastClass::Int {
                width: 8,
                signed: false,
            }),
            _ => None,
        }
    }

    /// The inclusive `[min, max]` domain of every representable value of
    /// this type, as `i128` -- what a `match`'s interval-exhaustiveness
    /// check (`crate::exhaustiveness`) treats as "the whole domain" a
    /// numeric/`bool`/`char` match must cover. `None` for every other
    /// type: `match` support is scoped to enums, integers, `bool`, and
    /// `char` for now.
    ///
    /// `Char`'s domain is `0..=0x10FFFF` (`char::MAX`) and does *not*
    /// carve out the surrogate hole (`0xD800..=0xDFFF`), since pointer
    /// reinterprets can manufacture such a value -- a deliberately
    /// conservative interval abstraction that may demand an arm for an
    /// unsupported value but never accepts an incomplete match. `pointer_
    /// bits` is the target's pointer width -- `ISize`/`USize` domains
    /// follow it.
    pub fn integer_domain(&self, pointer_bits: u32) -> Option<(i128, i128)> {
        Some(match self {
            Self::Bool => (0, 1),
            Self::Char => (0, char::MAX as i128),
            Self::I8 => (i8::MIN as i128, i8::MAX as i128),
            Self::I16 => (i16::MIN as i128, i16::MAX as i128),
            Self::I32 => (i32::MIN as i128, i32::MAX as i128),
            Self::I64 => (i64::MIN as i128, i64::MAX as i128),
            Self::ISize if pointer_bits == 32 => (i32::MIN as i128, i32::MAX as i128),
            Self::ISize => (i64::MIN as i128, i64::MAX as i128),
            Self::U8 => (u8::MIN as i128, u8::MAX as i128),
            Self::U16 => (u16::MIN as i128, u16::MAX as i128),
            Self::U32 => (u32::MIN as i128, u32::MAX as i128),
            Self::U64 => (u64::MIN as i128, u64::MAX as i128),
            Self::USize if pointer_bits == 32 => (u32::MIN as i128, u32::MAX as i128),
            Self::USize => (u64::MIN as i128, u64::MAX as i128),
            _ => return None,
        })
    }

    /// The same type with any statically-known enum-variant refinement
    /// erased (`MyEnum::Second` -> `MyEnum`) -- what inference positions
    /// that must stay variant-agnostic (an `if`'s unified branch type, an
    /// array literal's element type) normalize to. Shallow on purpose:
    /// refinement only ever exists at the top level of a value's type.
    pub fn widened(&self) -> ResolvedType {
        match self {
            Self::Enum {
                cell,
                variant: Some(_),
            } => Self::Enum {
                cell: cell.clone(),
                variant: None,
            },
            other => other.clone(),
        }
    }

    /// The canonical identity used by conformance and primitive
    /// registries. Lookup is about which methods belong to a type, not
    /// transient facts carried by a particular expression -- enum
    /// refinements and pointer-like mutability are such facts, so registry
    /// users must always key through this method.
    pub fn lookup_key(&self) -> ResolvedType {
        match self {
            Self::Enum { cell, .. } => Self::Enum {
                cell: cell.clone(),
                variant: None,
            },
            Self::Pointer { pointee, .. } => Self::Pointer {
                pointee: Box::new(pointee.lookup_key()),
                mutable: false,
            },
            Self::Slice { item, .. } => Self::Slice {
                item: Box::new(item.lookup_key()),
                mutable: false,
            },
            Self::Str { .. } => Self::Str { mutable: false },
            other => other.clone(),
        }
    }

    /// Whether a value of type `value` can be supplied where `self` is
    /// expected: exact equality, plus the one implicit widening this type
    /// system has -- a variant-refined enum value (`MyEnum::Second`) is
    /// usable as its plain enum. Never the reverse. Widening also applies
    /// through one level of *immutable* pointer/slice indirection
    /// (`*MyEnum::Second` usable as `*MyEnum`), but deliberately **not**
    /// through mutable ones (`*mut MyEnum::Second` never widens) -- see
    /// findings for why. A mutable pointer/slice does freely coerce into
    /// an immutable one of the same pointee, symmetric with a mutable
    /// binding being just as readable as an immutable one.
    pub fn accepts(&self, value: &ResolvedType) -> bool {
        if self == value {
            return true;
        }
        match (self, value) {
            (
                Self::Enum {
                    cell: expected,
                    variant: None,
                },
                Self::Enum {
                    cell: found,
                    variant: Some(_),
                },
            ) => expected.borrow().id == found.borrow().id,
            (
                Self::Pointer {
                    pointee: expected,
                    mutable: false,
                },
                Self::Pointer { pointee: found, .. },
            ) => expected.accepts(found),
            (
                Self::Slice {
                    item: expected,
                    mutable: false,
                },
                Self::Slice { item: found, .. },
            ) => expected.accepts(found),
            (Self::Array(expected, false), Self::Array(found, _)) => expected.accepts(found),
            // No `item` to recurse on, unlike `Slice` above -- and
            // deliberately its own arm, not folded into `Slice`'s: `*str`
            // and `*[u8]` must never accept one another implicitly (see
            // `Str`'s own doc comment), only `*mut str` -> `*str` widening.
            (Self::Str { mutable: false }, Self::Str { .. }) => true,
            _ => false,
        }
    }

    /// The module and declaration this type's own members belong to --
    /// what a member-visibility check needs. `None` for anything with no
    /// declaration of its own (a primitive, a slice, a pointer): their
    /// members come from a `primitive` block or a `conform`, whose
    /// visibility is the declaring spec's rather than the target's.
    pub fn declaring_owner(&self) -> Option<(Vec<Ident>, HirId)> {
        match self {
            Self::Struct(cell) => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            Self::Union(cell) => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            Self::Enum { cell, .. } => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            _ => None,
        }
    }

    /// This type with at most one pointer hop removed -- the seamless
    /// autoderef every member lookup applies (`ptr.field`, `ptr.method()`),
    /// never more than one level: `**Struct` still needs an explicit deref.
    pub fn autoderef(&self) -> &ResolvedType {
        match self {
            Self::Pointer { pointee, .. } => pointee,
            other => other,
        }
    }

    /// The `mutable` flag of any pointer-shaped type (`Pointer`/`Slice`/
    /// `Str`/`Array`) -- `None` for anything else. Lets a single check
    /// apply uniformly across all four instead of duplicating it per shape.
    pub fn pointer_like_mutable(&self) -> Option<bool> {
        match self {
            Self::Pointer { mutable, .. } | Self::Slice { mutable, .. } | Self::Str { mutable } => {
                Some(*mutable)
            }
            Self::Array(_, mutable) => Some(*mutable),
            _ => None,
        }
    }
}
