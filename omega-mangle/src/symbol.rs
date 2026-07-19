//! The mangler's own small, self-contained description of "the thing
//! being named" -- deliberately decoupled from `omega_analyzer`'s
//! `ResolvedType`/`CheckedFunctionDef` (which this crate never depends
//! on): nominal types are referenced by path + generic args only, never
//! by unrolling their fields, so there's nothing here that can recurse
//! through a self-referential type (a struct containing a pointer to
//! itself, e.g. `GenericNode<T>`'s `next: *GenericNode<T>`, mangles as
//! `Pointer(Named(Generic(path, [T])))` -- finite, since `Named` never
//! looks past the type's own name and arguments).
//!
//! Every type here derives structural `Hash`/`Eq`, which is exactly what
//! the compressor (see `crate::encode`) needs to detect "this exact
//! component already occurred" -- there is no `Rc`/identity anywhere in
//! this module, unlike `ResolvedType::Struct`'s `Rc<RefCell<_>>`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// struct/enum/union/spec
    Type,
    /// function/method/static
    Value,
}

impl Namespace {
    pub(crate) fn tag(self) -> char {
        match self {
            Namespace::Type => 't',
            Namespace::Value => 'v',
        }
    }

    pub(crate) fn from_tag(c: char) -> Option<Self> {
        match c {
            't' => Some(Namespace::Type),
            'v' => Some(Namespace::Value),
            _ => None,
        }
    }
}

/// A path to a named entity -- a module, a struct/enum/union/spec, or a
/// function/method. Always built bottom-up from `Root`, and every prefix
/// (not just the whole thing) is independently a valid, substitutable
/// `ManglePath` in its own right (see `crate::encode`'s compression pass).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ManglePath {
    /// The first module-path segment (an extern/entry module's own name).
    Root(String),
    /// `<parent>::<ident>`, tagged with `ident`'s own namespace.
    Nested(Box<ManglePath>, Namespace, String),
    /// `<parent><Args...>` -- concrete generic arguments applied to a
    /// path (a generic struct/enum/union/spec instantiation, or a
    /// generic free function/method instantiation).
    Generic(Box<ManglePath>, Vec<MangleType>),
}

/// Every shape a mangled type can take. Basic (primitive) types are not
/// substitutable (see `crate::encode::is_substitutable`) -- they're
/// already maximally short, so a backref to one would never be shorter
/// than just repeating it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MangleType {
    Void,
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
    F32,
    F64,
    /// `*T` (`false`) / `*mut T` (`true`)
    Pointer(Box<MangleType>, bool),
    /// `*[T]` (`false`) / `*mut [T]` (`true`)
    Slice(Box<MangleType>, bool),
    /// `[T]` -- a decayed, unsized array parameter.
    Array(Box<MangleType>),
    /// `[T; N]`
    SizedArray(Box<MangleType>, u64),
    /// `spec *T` (`false`) / `spec *mut T` (`true`) -- the inner type is
    /// always a `Named(Generic(spec_path, type_args))` (or plain
    /// `Named(spec_path)` for a non-generic spec), reusing the ordinary
    /// generic-args machinery rather than inventing a separate one.
    SpecObject(Box<MangleType>, bool),
    /// `fn(params...) -> return`, `is_variadic` mirrors
    /// `ResolvedFunctionType::is_variadic`. A function *type*'s own
    /// `self_mode` (when it denotes a method's own type, e.g. as a spec
    /// vtable slot) is captured the same way it is everywhere else here
    /// -- as an ordinary leading parameter, not a separate tag.
    Function(Vec<MangleType>, Box<MangleType>, bool),
    /// A struct/enum/union/spec, referenced by path (possibly with
    /// generic args via `ManglePath::Generic`). `Some(variant_index)`
    /// for a refined enum type (`ResolvedType::Enum { variant: Some(_),
    /// .. }`, e.g. a written `MyEnum::Second` annotation) -- omitted
    /// (`None`) for the ordinary, unrefined case.
    Named(ManglePath, Option<u32>),
}

/// The full identity of one mangled symbol. `signature` is `None` for a
/// non-function item (a struct/enum/union/spec's own path never needs
/// one); `Some((params, return_type))` for a function/method, with
/// `self` included in `params` as an ordinary leading entry whenever the
/// item is a method (its `MangleType` -- `Named(owner)` for by-value,
/// `Pointer(Named(owner), mutable)` for by-pointer -- already spells out
/// which self-mode was used; see `omega-mangle` crate docs).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub path: ManglePath,
    pub signature: Option<(Vec<MangleType>, MangleType)>,
    /// A general escape hatch mirroring RFC 2603's own
    /// `<vendor-specific-suffix>`, appended as `.` + this string -- meant
    /// for a piece of *external* tooling to disambiguate an
    /// already-complete symbol further (e.g. an LTO pass appending
    /// `.llvm.1234`), not for the compiler's own routine output: the
    /// RFC's own motivation section flags `.` as a real cross-platform
    /// portability problem, so nothing this compiler emits on its own
    /// should rely on this (e.g. `omega_codegen`'s vtable symbols use an
    /// ordinary nested identifier instead -- see its own `vtable_symbol`
    /// doc comment).
    pub vendor_suffix: Option<String>,
}
