use crate::checked::Storage;
use crate::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::{Ident, Path, SimpleSpan, Type};
use std::fmt;

/// A concrete cross-module lookup result -- either a type (a struct, found
/// via a qualified type reference) or a value (a function/extern/global,
/// found via a qualified place). This is deliberately the same shape
/// `VarBinding`/`find_defined_type` already split into locally; a foreign
/// lookup just needs both possibilities in one enum instead of two separate
/// local tables, since the caller doesn't yet know which kind it is.
#[derive(Debug, Clone)]
pub enum ResolvedItem {
    Type(ResolvedType),
    Value { r#type: ResolvedType, storage: Storage, decl_id: HirId },
}

/// What an `import` statement's path actually names -- not decidable from
/// its syntax alone (`import a::b::c;` is identical text whether `c` is a
/// submodule of `a::b` or an item inside it), so this is the answer a
/// `ModuleResolver` gives back after actually checking the module tree.
#[derive(Debug, Clone)]
pub enum ImportTarget {
    /// `path` names a real module -- the imported name binds to that whole
    /// namespace (`import mymodule;` then `mymodule::thing::foo()`).
    Module(Vec<Ident>),
    /// `path`'s last segment names an item inside the module formed by the
    /// rest of the path -- the imported name binds directly to that item
    /// (`import mymodule::foo;` then bare `foo()`).
    Item(ResolvedItem),
    /// `path`'s last segment names a *generic* item (struct or function) --
    /// unlike `Item`, this is never eagerly resolved to a `ResolvedItem`:
    /// importing supplies no type arguments (those only ever appear at a use
    /// site, e.g. `List<u32>` or `sum_generic(1, 2)`), so there is nothing
    /// concrete to build yet. Just the absolute path, to be substituted in
    /// for the alias wherever it's later referenced with concrete arguments
    /// (see `Context::generic_aliases`).
    GenericItem(Vec<Ident>),
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    UnknownModule(Vec<Ident>),
    UnknownItem { module: Vec<Ident>, item: Ident },
    NotVisible { module: Vec<Ident>, item: Ident },
    /// A module's signature transitively requires its own, still-in-progress
    /// signature (e.g. two structs in different modules referencing each
    /// other by value) -- `path` is the cycle, in the order it was
    /// discovered, ending back where it started.
    Cycle(Vec<Vec<Ident>>),
    /// Two filesystem entries (a file and a directory) claim the same
    /// module name at the same level.
    AmbiguousModule(Vec<Ident>),
    /// `path` resolved to a real file, but reading or parsing it failed --
    /// an I/O error, or a syntax error in the imported file itself.
    LoadFailed { path: Vec<Ident>, message: String },
    /// `item` (in `module`) is a struct that includes itself, directly or
    /// through one or more other structs -- possibly in other modules --
    /// entirely by value, with no pointer anywhere along the cycle. Such a
    /// type has no finite size (the same shape Rust rejects as E0072); this
    /// is the one global, item-granular query
    /// (`omega_driver::Driver::ensure_item`) replaces the old module-
    /// granularity `Cycle` above for -- see its doc comment for why a
    /// *pointer* reference to something still being resolved is never an
    /// error, only a direct, by-value one.
    RecursiveTypeWithoutIndirection { module: Vec<Ident>, item: Ident },
    /// `item` (in `module`) failed its own signature/body analysis -- the
    /// real diagnostics were already recorded against that module elsewhere
    /// (see `omega_driver::Driver::module_errors`); this is just a
    /// lightweight marker so a *reference* to the failed item can itself
    /// fail cleanly, without duplicating or re-deriving the underlying
    /// error here.
    ItemFailed { module: Vec<Ident>, item: Ident },
    /// `item` (in `module`) declares `expected` generic parameters, but was
    /// referenced with `found` type arguments -- covers both a generic item
    /// referenced with no arguments at all (a bare `Type::Named`, `found:
    /// 0`) and a `Type::Generic`/instantiation with the wrong count.
    GenericArgCountMismatch { module: Vec<Ident>, item: Ident, expected: usize, found: usize },
}

fn join(path: &[Ident]) -> String {
    path.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join("::")
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModule(path) => write!(f, "no such module '{}'", join(path)),
            Self::UnknownItem { module, item } => {
                write!(f, "module '{}' has no item '{}'", join(module), item.as_ref())
            }
            Self::NotVisible { module, item } => {
                write!(f, "'{}::{}' is not visible here", join(module), item.as_ref())
            }
            Self::Cycle(path) => write!(
                f,
                "cyclic module dependency: {}",
                path.iter().map(|p| join(p)).collect::<Vec<_>>().join(" -> ")
            ),
            Self::AmbiguousModule(path) => write!(
                f,
                "module '{}' is ambiguous (both a file and a directory claim this name)",
                join(path)
            ),
            Self::LoadFailed { path, message } => {
                write!(f, "failed to load module '{}': {message}", join(path))
            }
            Self::RecursiveTypeWithoutIndirection { module, item } => write!(
                f,
                "recursive type '{}::{}' has infinite size -- insert a pointer somewhere in the cycle to fix this",
                join(module),
                item.as_ref()
            ),
            Self::ItemFailed { module, item } => {
                write!(f, "'{}::{}' failed to resolve", join(module), item.as_ref())
            }
            Self::GenericArgCountMismatch { module, item, expected, found } => write!(
                f,
                "'{}::{}' expects {expected} type argument(s), found {found}",
                join(module),
                item.as_ref()
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// What `omega-analyzer` needs from the outside world to resolve anything
/// module-qualified. Everything module-tree-shaped -- submodule-vs-item
/// disambiguation at each `::`, filesystem lookups, cross-module caching,
/// cycle detection -- lives entirely in the implementation (`omega-driver`);
/// this crate never sees a filesystem or a cache, only ever asks these two
/// questions.
pub trait ModuleResolver {
    /// Called once per `import` statement while collecting signatures or
    /// analyzing bodies for a module.
    fn resolve_import(&mut self, path: &Path) -> Result<ImportTarget, ResolveError>;

    /// Called for *any* named-type or place reference that isn't satisfied
    /// by a local (function-body-level) scope -- including a same-module
    /// top-level reference, with `absolute_path`'s module prefix supplied
    /// implicitly by the caller. There is no longer an architectural
    /// difference between "same-module" and "cross-module" here; both are
    /// this one query, item-granular and memoized
    /// (`omega_driver::Driver::ensure_item`).
    ///
    /// `indirect` is true whenever the reference sits somewhere that never
    /// embeds its referent inline into another type's layout -- behind a
    /// pointer, or a function's own param/return types -- as opposed to a
    /// struct field or `SizedArray` element, which do. This is what lets a
    /// self/mutually-referencing *pointer* field (anywhere, even across
    /// modules) resolve while it's still mid-collection, while a direct,
    /// by-value reference to something still mid-collection is rejected as
    /// `ResolveError::RecursiveTypeWithoutIndirection` (a genuine
    /// infinite-size type) instead of silently built.
    ///
    /// `type_args` is the concrete substitution for a generic item's own
    /// declared type parameters -- empty for an ordinary, non-generic item
    /// (the overwhelmingly common case; every non-generic call site passes
    /// `&[]`), or the arguments a generic reference was instantiated with
    /// (`List<u32>`'s `[u32]`, or a generic function call's argument-deduced
    /// substitution -- see `Analyzer::resolve_generic_call`). A count
    /// mismatch against the item's own declared generic parameter list
    /// (including a non-empty declared list against an empty `type_args`,
    /// i.e. a bare reference to a generic item with no arguments at all) is
    /// `ResolveError::GenericArgCountMismatch`.
    fn resolve_item(
        &mut self,
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError>;

    /// A *raw*, unresolved view of a generic function's own declared
    /// signature -- just enough for duck-typed argument-driven type
    /// inference at a call site (see `Analyzer::resolve_generic_call`), with
    /// no analysis triggered and no instantiation attempted. `Ok(None)` for
    /// anything that isn't a generic function -- including a non-generic
    /// item, a generic *struct*, or a name that doesn't resolve at all --
    /// deferring all of those diagnoses to the ordinary (non-generic) call
    /// path, which re-derives them identically.
    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError>;
}

/// See `ModuleResolver::generic_function_signature`.
#[derive(Debug, Clone)]
pub struct GenericSignature {
    pub generics: Vec<Ident>,
    pub params: Vec<Type>,
}

/// The only variant the parser can produce today (no `pub`/`priv` keyword
/// exists yet) -- see `SignatureEntry`'s doc comment for why this field
/// exists at all despite always holding this one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
}

/// One resolved top-level item, as `omega_driver::Driver` records it in its
/// global `resolved_items` table. Carries a `Visibility` even though every
/// entry produced today is `Public` -- enforcing real privacy later is "stop
/// hardcoding `Public` here and stop skipping the check in `resolve_item`,"
/// not a data-model change.
#[derive(Debug, Clone)]
pub struct SignatureEntry {
    pub visibility: Visibility,
    pub item: ResolvedItem,
}

/// One `import` statement, already resolved to what its path actually names
/// -- `omega_driver::Driver` computes a module's whole list of these exactly
/// once (cycle-guarded: resolving one module's item-style imports can itself
/// need another module's -- see its `imports` cache), then hands the same
/// `Rc<[ResolvedImport]>` to every throwaway `Analyzer` built for one of that
/// module's items, which applies it fresh at construction (`Analyzer::new`).
/// `id`/`span` are the originating `HirImport`'s, kept alongside so a
/// duplicate alias can still be reported against the right source location.
#[derive(Debug, Clone)]
pub struct ResolvedImport {
    pub id: HirId,
    pub span: SimpleSpan,
    pub alias: Ident,
    pub target: ImportTarget,
}
