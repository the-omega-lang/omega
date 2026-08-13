//! Where module content comes from on disk: the local project's own root
//! directory, and every registered `--extern` dependency's own root --
//! both eagerly, structurally discovered in full at construction (see
//! [`fs_resolve::discover_tree`]), so every module *path* any extern
//! contains is already known upfront (`extern_modules`/`core_modules`),
//! and finding one (`ModuleRoots::locate`) is always a plain map lookup,
//! never a live filesystem access, local or extern alike.
//!
//! What gets done with a known path's *content* still varies by how
//! "eager" its owner is: the local project's own modules are always fully
//! parsed, signature-resolved, *and* body-checked (`Driver::
//! local_module_paths`/`collect_signatures`/`check_bodies`); an extern
//! module's own struct/spec *signatures* are now eagerly resolved too,
//! whichever extern it belongs to (`Driver::collect_extern_signatures`)
//! -- but never its body, and never anything reached only through an
//! ordinary reference (a free function, an overload, ...), which stays
//! exactly as on-demand as ever, parsed and resolved the first time
//! something actually needs it (`Driver::parse_module`/`ensure_item`).
//!
//! This is the *only* place a module path is turned into a filesystem
//! lookup: everything above it deals in declared module paths exclusively.

use crate::error::CompileError;
pub(crate) const CORE_MODULE: &str = "core";

pub(crate) fn is_core_module(path: &[Ident]) -> bool {
    path.first().map(Ident::as_ref) == Some(CORE_MODULE)
}
use crate::ModulePath;
use crate::fs_resolve::{self, ModuleLocation};
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
/// (`ModuleRoots::extern_trees`), a path within it is always found via a
/// plain map lookup (`ModuleRoots::locate`), but its content is only ever
/// *parsed*/*resolved* on demand -- eagerly for its struct/spec surface
/// (`Driver::collect_extern_signatures`), lazily for everything else,
/// never for its body.
pub struct ExternRoot {
    pub name: Ident,
    pub dir: PathBuf,
}

/// Every filesystem root this compilation may resolve a module path
/// against: the local project's own, and every `--extern`'s -- both
/// eagerly discovered in full, once, at construction, and both found via a
/// plain map lookup (`ModuleRoots::locate`) from then on. Turning a
/// discovered path into actual content still differs: the local tree is
/// fully parsed and compiled by `Driver::compile`; an extern's is parsed
/// and signature-resolved for every struct/spec eagerly
/// (`Driver::collect_extern_signatures`), and otherwise still resolved
/// lazily, on demand, the first time something actually references it.
pub(crate) struct ModuleRoots {
    /// Every module reachable under the local project's own root directory,
    /// discovered once, eagerly, at construction
    /// (`fs_resolve::discover_tree`) -- the filesystem *is* the source of
    /// truth for what exists in the package being compiled, so this is a
    /// complete inventory, not a cache of paths asked about so far. An
    /// absent key is a real, checked fact ("does not exist"), not "wasn't
    /// looked up yet".
    local_tree: HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    /// The local package's own root directory, kept so a package that turns
    /// out to contain no modules at all can name itself and the file it was
    /// looked for (`CompileError::EmptyPackage`). Nothing resolves through
    /// this -- `local_tree` above is the inventory; this is for diagnostics.
    local_dir: PathBuf,
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
    /// did (for ambient bare-name resolution and primitive discovery,
    /// both still `core`-exclusive), but the discovery mechanism itself has
    /// no reason to stay special-cased once every extern gets it anyway.
    extern_trees: IndexMap<Ident, HashMap<ModulePath, Result<ModuleLocation, ResolveError>>>,
}

impl ModuleRoots {
    /// `local` is the local project's own root directory, eagerly walked in
    /// full right here; `local_name` is an optional `--name=` override.
    /// Discovery always starts from the on-disk basename, then an override
    /// relabels the entire root subtree. There is no filename-based entry
    /// fallback: the root module is always the package module.
    ///
    /// Fails immediately if two package roots claim the same declared name.
    /// The name is the first segment of every absolute module path now, so
    /// both duplicate externs and a local/extern collision are genuinely
    /// ambiguous lookup keys.
    pub fn new(
        local: PathBuf,
        local_name: Option<Ident>,
        externs: Vec<ExternRoot>,
    ) -> Result<Self, Vec<CompileError>> {
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

        let mut local_tree = fs_resolve::discover_tree(&local);
        if let Some(name) = &local_name {
            local_tree = fs_resolve::relabel_root(local_tree, name);
        }
        if let Some(local_identity) = local_tree
            .keys()
            .find(|path| path.len() == 1)
            .and_then(|path| path.first())
            && let Some(extern_root) = registered.get(local_identity)
        {
            return Err(vec![CompileError::DuplicateModuleIdentity {
                name: local_identity.clone(),
                first: local,
                second: extern_root.dir.clone(),
            }]);
        }
        // Unconditional here: `relabel_root` is already a
        // no-op whenever an extern's declared name matches its own
        // on-disk basename (the common case -- `core`, `mathlib`, and
        // every existing extern), and there's no fallback convention to
        // protect for an extern the way there is for the local package.
        let extern_trees = registered
            .iter()
            .map(|(name, root)| {
                (
                    name.clone(),
                    fs_resolve::relabel_root(fs_resolve::discover_tree(&root.dir), name),
                )
            })
            .collect();
        Ok(Self {
            local_tree,
            local_dir: local,
            externs: registered,
            extern_trees,
        })
    }

    /// Whether `path` names an *extern* module -- a pure function of its own
    /// first segment, no separate bookkeeping needed: `import
    /// extern::<name>::...` always resolves to that project's own declared
    /// module name as the resulting absolute path's first segment (see
    /// `Driver::import_absolute_path`), and every module reachable *from* an
    /// extern module keeps that same segment leading (relative/root-rooted
    /// imports only ever extend a path, never replace its prefix).
    pub fn is_extern(&self, path: &[Ident]) -> bool {
        path.first()
            .is_some_and(|head| self.externs.contains_key(head))
    }

    /// Whether `name` was registered via `--extern` at all -- what
    /// `import extern::<name>;` checks before trying to locate anything, so a
    /// typo'd or forgotten flag gets its own precise diagnostic.
    pub fn has_extern(&self, name: &Ident) -> bool {
        self.externs.contains_key(name)
    }

    /// Finds `path`'s own content and children on disk (see
    /// [`ModuleLocation`]). Both a local and an extern path are answered
    /// straight out of their own eager inventory (`local_tree`/
    /// `extern_trees`) built at construction -- a plain map lookup, no live
    /// filesystem access, and no possibility of "doesn't exist yet, but
    /// might once something imports it": the whole point of eager
    /// discovery, local or extern, is that this is already a complete,
    /// structural fact. Extern resolution used to re-derive this live, one
    /// path at a time, via `fs_resolve::locate_module` -- made redundant by
    /// `extern_trees` already knowing everything eagerly (a consequence of
    /// every extern's struct/spec surface now being eagerly resolved too,
    /// see `Driver::collect_extern_signatures`), and actively wrong once a
    /// root's declared name can differ from its on-disk basename: a live
    /// lookup would search for a literal `<declared-name>.omg` on disk,
    /// which `relabel_root` deliberately no longer guarantees exists.
    pub fn locate(&self, path: &[Ident]) -> Result<ModuleLocation, ResolveError> {
        let tree = if self.is_extern(path) {
            &self.extern_trees[&path[0]]
        } else {
            &self.local_tree
        };
        match tree.get(path) {
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
    /// The local package's own root directory, paired with the module file
    /// that root would have to contain to be a package at all -- only ever
    /// read to build `CompileError::EmptyPackage`.
    pub fn local_root(&self) -> (PathBuf, PathBuf) {
        let expected = match fs_resolve::basename(&self.local_dir) {
            Some(name) => self.local_dir.join(format!("{}.omg", name.as_ref())),
            None => self.local_dir.clone(),
        };
        (self.local_dir.clone(), expected)
    }

    pub fn local_modules(
        &self,
    ) -> impl Iterator<Item = (&ModulePath, &Result<ModuleLocation, ResolveError>)> {
        self.local_tree.iter()
    }

    /// Every real module (`own_file: Some`) in a tree -- the shared filter
    /// `core_modules`/`extern_modules` both apply, kept as one place rather
    /// than duplicated across them.
    fn real_modules(
        tree: &HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    ) -> Vec<ModulePath> {
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
    /// `docs/10-modules-and-linkage.md`), and the one package `primitive` blocks
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
        self.extern_trees
            .values()
            .flat_map(Self::real_modules)
            .collect()
    }
}
