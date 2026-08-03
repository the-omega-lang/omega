//! Where module content comes from on disk: the local project's own root
//! directory (eagerly, structurally discovered in full -- see
//! [`fs_resolve::discover_tree`]) and every registered `--extern`
//! dependency's own root (resolved lazily, on demand, one path at a time --
//! it is never the package *being compiled*).
//!
//! This is the *only* place a module path is turned into a filesystem
//! lookup: everything above it deals in declared module paths exclusively.

use crate::error::CompileError;
use crate::fs_resolve::{self, ModuleLocation, locate_module};
use crate::ModulePath;
use indexmap::IndexMap;
use omega_analyzer::resolver::ResolveError;
use omega_parser::prelude::Ident;
use std::collections::HashMap;
use std::path::PathBuf;

/// One `--extern` flag: `name` is this module's declared identity (by
/// default the registered directory's own basename, or an explicit
/// override from `--extern=<name>:<dir>`) and doubles as both what `import
/// extern::<name>;` selects it with *and* its real ABI/mangling identity --
/// there is no separate local alias. `dir` is its own search root -- an
/// extern's root is just someone else's project root, resolved exactly the
/// same way the local project's is, just never eagerly walked (see
/// `ModuleRoots::locate`).
pub struct ExternRoot {
    pub name: Ident,
    pub dir: PathBuf,
}

/// Every filesystem root this compilation may resolve a module path
/// against: the local project's own (eagerly discovered in full, once, at
/// construction) and every `--extern` (resolved lazily, on demand,
/// scanned but never eagerly compiled -- see `Driver::compile`).
pub(crate) struct ModuleRoots {
    /// Every module reachable under the local project's own root directory,
    /// discovered once, eagerly, at construction
    /// (`fs_resolve::discover_tree`) -- the filesystem *is* the source of
    /// truth for what exists in the package being compiled, so this is a
    /// complete inventory, not a cache of paths asked about so far. An
    /// absent key is a real, checked fact ("does not exist"), not "wasn't
    /// looked up yet".
    local_tree: HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    /// Every `--extern`, keyed by its declared name, in registration order.
    /// A module path whose first segment is a key here is an *extern*
    /// module: resolved against that entry's own `dir` instead of the local
    /// tree above.
    externs: IndexMap<Ident, ExternRoot>,
}

impl ModuleRoots {
    /// `local` is the local project's own root directory, eagerly walked in
    /// full right here. Fails immediately if two different `--extern`
    /// directories claim the same declared name -- genuinely ambiguous,
    /// since that name is a real lookup key (`extern::<name>::...`), unlike
    /// the local project's own declared identity, which never is (see
    /// `Driver::import_absolute_path`'s `ProjectRoot` arm: only an extern's
    /// own name is ever prepended to a path, never the local project's) --
    /// so a local/extern name collision isn't checked at all; it can't
    /// actually change how anything resolves.
    pub fn new(local: PathBuf, externs: Vec<ExternRoot>) -> Result<Self, Vec<CompileError>> {
        let mut registered: IndexMap<Ident, ExternRoot> = IndexMap::new();
        let mut errors = Vec::new();
        for root in externs {
            match registered.get(&root.name) {
                // Identical directory registered twice -- harmless, keep one.
                Some(existing) if existing.dir == root.dir => {}
                Some(existing) => errors.push(CompileError::DuplicateModuleIdentity {
                    name: root.name.clone(),
                    first: existing.dir.clone(),
                    second: root.dir,
                }),
                None => {
                    registered.insert(root.name.clone(), root);
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let local_tree = fs_resolve::discover_tree(&local);
        Ok(Self { local_tree, externs: registered })
    }

    /// Whether `path` names an *extern* module -- a pure function of its own
    /// first segment, no separate bookkeeping needed: `import
    /// extern::<name>::...` always resolves to that project's own declared
    /// module name as the resulting absolute path's first segment (see
    /// `Driver::import_absolute_path`), and every module reachable *from* an
    /// extern module keeps that same segment leading (relative/root-rooted
    /// imports only ever extend a path, never replace its prefix).
    pub fn is_extern(&self, path: &[Ident]) -> bool {
        path.first().is_some_and(|head| self.externs.contains_key(head))
    }

    /// Whether `name` was registered via `--extern` at all -- what
    /// `import extern::<name>;` checks before trying to locate anything, so a
    /// typo'd or forgotten flag gets its own precise diagnostic.
    pub fn has_extern(&self, name: &Ident) -> bool {
        self.externs.contains_key(name)
    }

    /// Finds `path`'s own content and children on disk (see
    /// [`ModuleLocation`]). A local path is answered straight out of the
    /// eager inventory built at construction -- a plain map lookup, no
    /// filesystem access, and no possibility of "doesn't exist yet, but
    /// might once something imports it": the whole point of eager local
    /// discovery is that this is already a complete, structural fact. An
    /// extern path is still resolved live, on demand, exactly as before --
    /// it's never the package being compiled, so it never earns the eager
    /// treatment (see this module's own doc comment).
    pub fn locate(&self, path: &[Ident]) -> Result<ModuleLocation, ResolveError> {
        if self.is_extern(path) {
            let root = &self.externs[&path[0]].dir;
            return locate_module(std::slice::from_ref(root), path);
        }
        match self.local_tree.get(path) {
            Some(result) => result.clone(),
            None => Err(ResolveError::UnknownModule(path.to_vec())),
        }
    }

    /// Whether `path` names a real module on disk at all -- the cheap check
    /// behind "is this an item import or a whole-module import".
    pub fn module_exists(&self, path: &[Ident]) -> bool {
        self.locate(path).is_ok()
    }

    /// The complete local inventory, exactly as discovered at construction
    /// -- `Driver::local_module_paths`' only reader, for turning "every
    /// module path under the local root" into "every module the local
    /// package's own build actually contains" (filtering out namespace-only
    /// directories, surfacing a genuine discovery failure like
    /// `AmbiguousModule` as a real error). Kept as a thin, unfiltered
    /// accessor here -- the same division of labor `structs()`/`spec_cells()`
    /// already follow elsewhere in this codebase -- rather than baking that
    /// filtering into `ModuleRoots` itself.
    pub fn local_modules(&self) -> impl Iterator<Item = (&ModulePath, &Result<ModuleLocation, ResolveError>)> {
        self.local_tree.iter()
    }
}

impl crate::Driver {
    /// Whether `name` names a real top-level module in the local project --
    /// either a flat `<name>.omg` file or a directory-shaped `<name>/`
    /// (nested content resolved the same way any other directory-shaped
    /// module's is). What `omgc` checks to find the entry module: the
    /// local project's own declared identity first, falling back to the
    /// fixed `main` convention.
    pub fn has_local_module(&self, name: &Ident) -> bool {
        self.roots.module_exists(std::slice::from_ref(name))
    }
}
