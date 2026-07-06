use crate::checked::Storage;
use crate::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::{Ident, Path};
use std::collections::HashMap;
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

    /// Called for a qualified path at a use site. `absolute_path` is already
    /// fully resolved to an absolute module path plus a final item name --
    /// the caller (`Context`) has already substituted its own import alias
    /// for the first segment. Always resolves to a concrete item; running
    /// out of path while still on a module, or landing on a non-public item,
    /// is an error here.
    fn resolve_item(&mut self, absolute_path: &[Ident]) -> Result<ResolvedItem, ResolveError>;
}

/// The only variant the parser can produce today (no `pub`/`priv` keyword
/// exists yet) -- see `SignatureEntry`'s doc comment for why this field
/// exists at all despite always holding this one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
}

/// One exported item, as recorded in a `ModuleSignature`. Carries a
/// `Visibility` even though every entry produced today is `Public` --
/// enforcing real privacy later is "stop hardcoding `Public` here and stop
/// skipping the check in `resolve_item`," not a data-model change.
#[derive(Debug, Clone)]
pub struct SignatureEntry {
    pub visibility: Visibility,
    pub item: ResolvedItem,
}

/// The result of `Analyzer::collect_signatures` for one module: every
/// top-level item's resolved signature (no bodies), keyed by name. This is
/// exactly what another module's `resolve_item` call ultimately reads from.
#[derive(Debug, Clone, Default)]
pub struct ModuleSignature {
    pub items: HashMap<Ident, SignatureEntry>,
}
