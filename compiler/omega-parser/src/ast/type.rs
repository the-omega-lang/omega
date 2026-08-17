use crate::ast::identifier::{Ident, Origin, Path};
use crate::diagnostics::Span;
use crate::ast::self_mode::SelfMode;

/// One `name: Type` parameter of a function, method, spec function, or
/// function *type*.
///
/// A parameter and a struct field are written identically, but they are not
/// the same thing: a field carries a visibility modifier and a parameter
/// cannot. Fields therefore stay `DeclarationStmt`, and this node exists so
/// the two parameter-list parsers -- one producing `DeclarationStmt`, one
/// producing a bare `(Ident, Type)` pair -- become one.
///
/// Equality ignores the spans and the macro `origin`, matching `Path`'s own
/// hand-written `PartialEq`: those are provenance, not syntax, and a
/// function type's identity must not depend on where it was written.
#[derive(Debug, Clone)]
pub struct Param {
    pub ident: Ident,
    /// The declared name only -- see `DeclarationStmt::name_span`.
    pub name_span: Span,
    /// The whole `name: Type`.
    pub span: Span,
    pub origin: Origin,
    pub r#type: Type,
}

impl PartialEq for Param {
    fn eq(&self, other: &Self) -> bool {
        self.ident == other.ident && self.r#type == other.r#type
    }
}

impl Eq for Param {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<Param>,
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
    /// `[]T` -- inferred-size. Standalone, this is always invalid: `Context::
    /// resolve_type` rejects it unconditionally wherever it's reached
    /// directly, since there's no length to give it. The only two legal
    /// uses are behind a leading `*` (`Pointer(InferredArray(T), mutable)`,
    /// which `Context::resolve_pointer_type` turns into `ResolvedType::
    /// Slice` -- a fat pointer value with a runtime length),
    /// or as a declaration's type annotation paired with an array-literal
    /// initializer, which infers the real length from it (see
    /// `Analyzer::resolve_typed_decl_init`). Mutability lives on the
    /// wrapping `*`/`*mut` sigil, never here -- `[]T` itself has no
    /// `mutable` flag, matching `SizedArray` below and unlike `Pointer`.
    InferredArray(Box<Type>),
    /// `[?]T` -- unsized. Standalone, this is always
    /// invalid, with no exception (unlike `InferredArray` above, it has no
    /// inferred-length declaration escape hatch either -- a slice's length
    /// is only ever known at runtime, nothing to infer at all). The only
    /// legal use is behind a leading `*` (`Pointer(UnknownSizeArray(T),
    /// mutable)`, which `Context::resolve_pointer_type` turns into
    /// `ResolvedType::Array`, a thin array-like pointer).
    UnknownSizeArray(Box<Type>),
    /// `[N]T` -- a sized, inline, contiguous run of exactly `N` `T`s. `N`
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
