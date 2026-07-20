use crate::checked::Storage;
use crate::resolved_type::{ResolvedFunctionType, ResolvedMethod, ResolvedType};
use omega_hir::HirId;
use omega_parser::prelude::{Ident, Type};
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
    /// `import extern::name::...;` where `name` wasn't registered via
    /// `--extern=name:path` on the command line.
    UnknownExtern(Ident),
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
    /// `path` parsed successfully, but macro expansion (`omega_parser::
    /// macros::expand`, run right after parsing and before HIR lowering)
    /// failed -- an undefined/misused macro, or one that expanded into
    /// invalid syntax.
    MacroExpansionFailed { path: Vec<Ident>, message: String },
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
    /// A bound generic (`T: Animal`) was instantiated with a concrete type
    /// that doesn't nominally implement `spec` -- `missing` names every
    /// spec function the type doesn't provide (own or default). Also used
    /// for a `spec *Animal` coercion from a concrete pointer whose pointee
    /// doesn't implement the spec.
    SpecNotImplemented { type_name: String, spec: Ident, missing: Vec<Ident> },
}

fn join(path: &[Ident]) -> String {
    path.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join("::")
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModule(path) => write!(f, "cannot find module '{}'", join(path)),
            Self::UnknownExtern(name) => write!(
                f,
                "no extern dependency named '{}' (missing --extern={}:<path>?)",
                name.as_ref(),
                name.as_ref()
            ),
            Self::UnknownItem { module, item } => {
                write!(f, "cannot find '{}' in module '{}'", item.as_ref(), join(module))
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
            Self::MacroExpansionFailed { path, message } => {
                write!(f, "macro expansion failed in module '{}': {message}", join(path))
            }
            Self::RecursiveTypeWithoutIndirection { module, item } => write!(
                f,
                "recursive type '{}::{}' has infinite size",
                join(module),
                item.as_ref()
            ),
            Self::ItemFailed { module, item } => {
                write!(f, "cannot use '{}::{}' because of its own error", join(module), item.as_ref())
            }
            Self::GenericArgCountMismatch { module, item, expected, found } => write!(
                f,
                "'{}::{}' expects {expected} type argument(s), found {found}",
                join(module),
                item.as_ref()
            ),
            Self::SpecNotImplemented { type_name, spec, missing } => write!(
                f,
                "'{type_name}' does not implement spec '{}' (missing: {})",
                spec.as_ref(),
                missing.iter().map(Ident::as_ref).collect::<Vec<_>>().join(", ")
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
    /// What `alias` means as an import in `module_path`, resolved lazily and
    /// memoized per `(module_path, alias)` pair (not per whole module) --
    /// the fix for a real false-cycle bug a whole-module-granular version of
    /// this used to have: two modules whose *unrelated* items happened to
    /// cross-import each other's module would deadlock resolving each
    /// other's *entire* import list, even though the specific items in
    /// question never referenced each other. `Ok(None)` means `module_path`
    /// has no `import` statement binding `alias` at all -- the caller's own
    /// "assume this name is my own module's item" fallback applies, exactly
    /// as if this had never been called. Called on demand, the first time a
    /// name lookup that isn't satisfied locally actually needs to know
    /// whether it's an import alias -- never eagerly for a module's whole
    /// import list up front (see `Analyzer::new`, which no longer takes a
    /// pre-resolved import list at all).
    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError>;

    /// Every alias a module's own `import` statements bind, purely for "did
    /// you mean" typo suggestions (`Context::similar_module_alias`) -- cheap
    /// and resolution-free (the raw alias *names* are known the moment a
    /// module is indexed, long before any of them are actually resolved).
    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident>;

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

    /// `name`'s every overload candidate in `module_path`, each already
    /// paired with the `HirId` a callee place root needs -- an escape hatch
    /// alongside `resolve_item` exactly like `generic_function_signature`
    /// is, and for the identical reason: an overloaded name can't be
    /// addressed by `resolve_item`'s single-result `(absolute_path,
    /// type_args)` key at all (nothing about the *name* alone picks one
    /// candidate; only the call's own argument types do, at the call
    /// site -- see `Analyzer::resolve_overloaded_call`). `Ok(None)` means
    /// "not an overloaded name" (zero or exactly one candidate) -- callers
    /// fall through to the ordinary `resolve_item` path unchanged in that
    /// case, so this never affects behavior for the overwhelmingly common
    /// non-overloaded name.
    fn function_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<(HirId, ResolvedFunctionType)>>, ResolveError>;


    /// Mints a fresh `HirId` with no corresponding HIR node of its own --
    /// used for a spec-default method instantiated for a concrete
    /// implementor that didn't override it (see
    /// `Analyzer::signature_of_struct`'s implements-clause resolution),
    /// exactly the same minting `omega_driver::Driver::compute_item`
    /// already does internally for a generic instantiation's own identity,
    /// surfaced here so `Analyzer` (which has no minting of its own) can
    /// request one too.
    fn fresh_synthetic_id(&mut self) -> HirId;

    /// The name of a top-level item in `module_path` most similar to
    /// `target` (see `crate::similarity::best_match`), drawn only from
    /// `namespace` -- the "did you mean" candidate for a reference that
    /// resolved to nothing. Only the resolver can answer this: the analyzer
    /// never holds a module-wide name list (items are resolved one at a
    /// time, on demand), so scope-level searches alone would miss every
    /// top-level item. Purely advisory (error path only): `None` when
    /// nothing is close enough, or when the module can't even be indexed.
    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident>;

    /// Every UFCS method a value of type `receiver` gets from `@ufcs`-
    /// flagged specs declared in `core` (see `crate::annotations::
    /// ItemKind::Spec`'s doc comment) -- called by `Analyzer::find_methods`
    /// (instance calls) and `Analyzer::resolve_type_member` (static/
    /// associated calls on a bare primitive), only once the ordinary
    /// lookup already came up empty. `Ok(vec![])` covers both "`core` isn't
    /// registered for this compilation at all" and "no `@ufcs` spec
    /// targets `receiver`" -- a receiver simply having no UFCS methods is
    /// never itself a resolution failure; `Err` is reserved for a genuine
    /// problem discovering/analyzing `core`'s own declarations.
    fn ufcs_methods(&mut self, receiver: &ResolvedType) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError>;
}

/// Which namespace a "did you mean" suggestion should draw from -- a type
/// position must never suggest a function, and vice versa (a wrong hint is
/// worse than none). See `ModuleResolver::similar_item_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemNamespace {
    /// Functions, globals, externs.
    Value,
    /// Structs (generic templates included).
    Type,
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

