use crate::ast::identifier::{Ident, Path};
use crate::ast::self_mode::SelfMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<(Ident, Type)>,
    pub return_type: Box<Type>,
    pub is_variadic: bool,
    /// `None` for an ordinary function type; `Some` for a member-function
    /// type, carrying exactly how `self` is spelled (`self`/`mut self`/
    /// `*self`/`*mut self`). See `SelfMode`.
    pub self_mode: Option<SelfMode>,
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
    /// `[T]` (immutable, `mutable: false`) or `mut [T]` (`mutable: true`) --
    /// an unsized run of `T`: genuinely just a thin pointer value with
    /// array-like properties (indexing, slicing via `&arr[a..b]`), no
    /// length carried alongside it. `mut` prefixes the whole `[...]`
    /// (rather than sitting just after a sigil, the way `*mut T` does) --
    /// `[T]` has no leading sigil of its own to attach it to; see
    /// `parser::r#type::parse_type`'s own doc comment. Mutability is a
    /// type-level fact here exactly the way it is for `Pointer` above --
    /// whether `arr[i] = x` is legal follows `[T]`'s own declared
    /// mutability, never whatever binding happens to hold the value, the
    /// same directional rule `*T`/`*mut T` already enforces. `*[T]` is the
    /// pointer-*to*-this form and is *not* `Pointer(Array(T))` -- see
    /// `Context::resolve_type`'s special case, which turns that
    /// combination into `ResolvedType::Slice` (a fat pointer) instead, per
    /// the language's actual slice design; `[T]` never needs a leading `*`
    /// to function as a value type, unlike `*T` pointing *at* a `T`.
    Array(Box<Type>, bool),
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
    /// `spec Animal` -- no `*`, unlike `SpecObject` above: a *static*-
    /// dispatch spec bound written in type position, Rust's `impl Trait`
    /// equivalent. In parameter position this is sugar for an implicit
    /// generic parameter bound by the named spec (desugared away entirely
    /// during HIR lowering -- the analyzer never sees this variant there).
    /// In the return position of a spec's own function declaration, or of
    /// an ordinary function, it means "some concrete type satisfying this
    /// spec, to be determined per implementor / inferred from the body" --
    /// never itself the type of a runtime value the way `SpecObject` is
    /// (there is no fat pointer, no vtable; every occurrence is resolved
    /// away to a genuine concrete type before codegen ever runs). The boxed
    /// `Type` is always a `Named`/`Generic` spec reference, exactly like
    /// `SpecObject`'s pointee.
    SpecStatic(Box<Type>),
}
