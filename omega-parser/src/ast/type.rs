use crate::ast::identifier::{Ident, Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<(Ident, Type)>,
    pub return_type: Box<Type>,
    pub is_variadic: bool,
    pub is_member_function: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// Identifier types, possibly module-qualified. Example: `void`, `i32`,
    /// `mymodule::Foo`.
    Named(Path),
    /// `*T` (immutable, `mutable: false`) or `*mut T` (`mutable: true`) --
    /// whether the pointee may be written through. Immutable by default,
    /// matching every binding's own default (see `DeclarationStmt::mutable`).
    Pointer(Box<Type>, bool),
    Function(FunctionType),
    /// `[T]` -- an unsized run of `T`, only ever meaningful today as a
    /// parameter type used the way C's decayed array parameters are (see
    /// `argv : [*u8]` in `examples/dev/main.omg`): a single thin pointer
    /// value, with no length carried alongside it. `*[T]` is the pointer
    /// form of this and is *not* `Pointer(Array(T))` -- see
    /// `Context::resolve_type`'s special case, which turns that combination
    /// into `ResolvedType::Slice` (a fat pointer) instead, per the
    /// language's actual slice design.
    Array(Box<Type>),
    /// `[T; N]` -- a sized, inline, contiguous run of exactly `N` `T`s. `N`
    /// is kept as raw digit text here and parsed/range-checked during type
    /// resolution (`Context::resolve_type`), the same way `NumberExpr`'s
    /// integer literals are kept as text until semantic analysis -- the
    /// parser never rejects input on its own.
    SizedArray(Box<Type>, String),
    /// `Path<Type, ...>` -- a generic item (struct or function) referenced
    /// with explicit type arguments, e.g. `List<u32>`. Only ever produced
    /// where this parser already parses a named type (`<` never appears in
    /// expression grammar, so there's no ambiguity to disambiguate here).
    /// `Type::Named` stays the plain (non-generic) case -- unrelated to this
    /// one at the type level; only semantic analysis knows whether a given
    /// path actually names a generic item.
    Generic(Path, Vec<Type>),
    /// `spec *Animal` (immutable, `mutable: false`) or `spec *mut Animal`
    /// (`mutable: true`) -- a *dynamic-dispatch* trait-object pointer,
    /// unlike an ordinary `Pointer`: at runtime this is a fat pointer (a
    /// data pointer plus a compiler-generated vtable pointer), and the
    /// pointee's *concrete* type is erased -- only that it implements the
    /// named spec is known. The boxed `Type` is always a `Named`/`Generic`
    /// spec reference (e.g. `Animal`, `Iterator<i32>`), never itself a
    /// pointer. Contrast with a *static*-dispatch spec bound (`T: Animal`
    /// on a `GenericParam`), which stays a thin, ordinary pointer once `T`
    /// is monomorphized to a concrete type.
    SpecObject(Box<Type>, bool),
}
