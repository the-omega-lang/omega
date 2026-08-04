//! Where module content comes from on disk: the local project's own root
//! directory, and every registered `--extern` dependency's own root --
//! both eagerly, structurally discovered in full at construction (see
//! [`fs_resolve::discover_tree`]), so every module *path* any extern
//! contains is already known upfront (`extern_modules`/`core_modules`).
//! What gets done with a known path still varies by how "eager" its owner
//! is: the local project's own modules are always fully parsed, signature-
//! resolved, *and* body-checked (`Driver::local_module_paths`/
//! `collect_signatures`/`check_bodies`); an extern module's own struct/spec
//! *signatures* are now eagerly resolved too, whichever extern it belongs
//! to (`Driver::collect_extern_signatures`) -- but never its body, and
//! never anything reached only through an ordinary reference (a free
//! function, an overload, ...), which stays exactly as on-demand as ever
//! (`ModuleRoots::locate`/`Driver::ensure_item`).
//!
//! This is the *only* place a module path is turned into a filesystem
//! lookup: everything above it deals in declared module paths exclusively.

use crate::error::CompileError;
use crate::extensions::CORE_MODULE;
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
/// same way the local project's is: its own tree is eagerly *discovered*
/// (`ModuleRoots::extern_trees`) but only ever *parsed*/*resolved* on
/// demand -- for an ordinary reference one path at a time
/// (`ModuleRoots::locate`), for its struct/spec surface eagerly but never
/// its body (`Driver::collect_extern_signatures`).
pub struct ExternRoot {
    pub name: Ident,
    pub dir: PathBuf,
}

/// Every filesystem root this compilation may resolve a module path
/// against: the local project's own, and every `--extern`'s -- both
/// eagerly discovered in full, once, at construction. Turning a discovered
/// path into actual content still differs: the local tree is fully parsed
/// and compiled by `Driver::compile`; an extern's is parsed and
/// signature-resolved for every struct/spec eagerly
/// (`Driver::collect_extern_signatures`), and otherwise still resolved
/// lazily, on demand, one path at a time (`ModuleRoots::locate`).
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
    /// Every registered `--extern`'s own eager inventory, keyed the same as
    /// `externs` -- discovered once at construction exactly like
    /// `local_tree` is (see `Driver::collect_extern_signatures`, this
    /// field's main reader: every extern's own struct/spec surface is now
    /// eagerly resolved, not just `core`'s). `core_modules` reads its own
    /// entry out of this same map rather than getting a separate field --
    /// `core` needed eager *tree discovery* even before every other extern
    /// did (for ambient bare-name resolution and `for`-block discovery,
    /// both still `core`-exclusive), but the discovery mechanism itself has
    /// no reason to stay special-cased once every extern gets it anyway.
    extern_trees: IndexMap<Ident, HashMap<ModulePath, Result<ModuleLocation, ResolveError>>>,
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
        let extern_trees =
            registered.iter().map(|(name, root)| (name.clone(), fs_resolve::discover_tree(&root.dir))).collect();
        Ok(Self { local_tree, externs: registered, extern_trees })
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
    /// extern path is still resolved live, on demand, one path at a time --
    /// its *existence* is already eagerly known (`extern_trees`), but it's
    /// never the package being compiled, so a single lookup here still
    /// just re-derives the same `ModuleLocation` live rather than reading
    /// the inventory back (see this module's own doc comment).
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

    /// Every real module (`own_file: Some`) in a tree -- the shared filter
    /// `core_modules`/`extern_modules` both apply, kept as one place rather
    /// than duplicated across them.
    fn real_modules(tree: &HashMap<ModulePath, Result<ModuleLocation, ResolveError>>) -> Vec<ModulePath> {
        tree.iter()
            .filter_map(|(path, result)| match result {
                Ok(location) if location.own_file.is_some() => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every real module in `core`'s own tree, however `core` happens to be
    /// registered for *this* compilation -- unlike every other package,
    /// `core` is always fully, eagerly known, whether it's the local
    /// package itself (a plain filter over `local_tree`, already fully
    /// known -- covers both "the local package's own identity is literally
    /// `core`" and "`core` is an ordinary nested module of a bigger local
    /// project" uniformly) or a registered `--extern` (its own entry in
    /// `extern_trees`). Empty if `core` isn't registered at all. This is
    /// what makes `core` a true, always-available prelude (see
    /// `docs/10-modules-and-linkage.md`), and the one package `for`-blocks
    /// may live in -- both still exclusive to `core`, unlike the eager
    /// *tree discovery* `extern_trees` itself now gives every extern.
    pub fn core_modules(&self) -> Vec<ModulePath> {
        match self.extern_trees.get(&Ident(CORE_MODULE.to_string())) {
            Some(tree) => Self::real_modules(tree),
            None => Self::real_modules(&self.local_tree)
                .into_iter()
                .filter(|path| path.first().map(Ident::as_ref) == Some(CORE_MODULE))
                .collect(),
        }
    }

    /// Every real module across *every* registered `--extern`'s own eager
    /// tree -- what `Driver::collect_extern_signatures` walks to eagerly
    /// resolve every extern's struct/spec surface, `core` included (`core`
    /// gets no special exemption here; its *other* privileges above stay
    /// exclusive, this one doesn't). The local package is never reached
    /// through here -- it already gets full signature *and* body treatment
    /// via `local_module_paths`/`collect_signatures`.
    pub fn extern_modules(&self) -> Vec<ModulePath> {
        self.extern_trees.values().flat_map(Self::real_modules).collect()
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
