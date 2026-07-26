//! Where module content comes from on disk: the local project's search
//! roots, every registered `--extern` dependency, and the declared-name ->
//! real-file translation an explicit `--name=`/`--extern=<name>:<file>`
//! override introduces.
//!
//! This is the *only* place a module path is turned into a filesystem
//! lookup: everything above it deals in declared module paths exclusively.

use crate::error::CompileError;
use crate::fs_resolve::{ModuleLocation, locate_module};
use crate::ModulePath;
use indexmap::IndexMap;
use omega_analyzer::resolver::ResolveError;
use omega_parser::prelude::Ident;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One `--extern` flag, already split into its parts.
///
/// `name` is this module's declared identity (by default `file`'s own stem,
/// or an explicit override from `--extern=<name>:<file>`) and doubles as both
/// what `import extern::<name>;` selects it with *and* its real ABI/mangling
/// identity -- there is no separate local alias.
///
/// `dir` is `file`'s own parent directory, the search root for this module
/// and its children (an extern file is just an entry file for someone else's
/// project). `file` is kept alongside so a collision can name the concrete
/// file, and so its *real* on-disk stem is still known when `name` was
/// overridden away from it (see [`ModuleRoots::physical_lookup_path`]).
pub struct ExternRoot {
    pub name: Ident,
    pub dir: PathBuf,
    pub file: PathBuf,
}

/// Two different files claiming the same declared module identity, recorded
/// when the second one is registered and reported by
/// [`ModuleRoots::resolve_identities`].
struct IdentityClash {
    name: Ident,
    first: PathBuf,
    second: PathBuf,
}

/// Every filesystem root this compilation may resolve a module path against.
///
/// Search roots are tried in order, first match wins -- deliberately dumb (no
/// per-root package identity/namespacing) so a real package system later just
/// means adding entries and namespacing logic behind this one type, not
/// touching any call site. There is exactly one local search root today (the
/// entry file's parent directory); an extern module is instead resolved
/// against its own registered root.
pub(crate) struct ModuleRoots {
    local: Vec<PathBuf>,
    /// Every `--extern`, keyed by its declared name, in registration order.
    /// A module path whose first segment is a key here is an *extern* module:
    /// resolved against that entry's own `dir` instead of `local`, and never
    /// eagerly compiled -- only scanned on demand (see `Driver::compile`).
    externs: IndexMap<Ident, ExternRoot>,
    /// Name collisions found while registering `externs`, held until
    /// `resolve_identities` can report them as real `CompileError`s.
    clashes: Vec<IdentityClash>,
    /// Every registered root's declared identity mapped to the file its
    /// content actually comes from -- empty until `resolve_identities` runs.
    /// Only [`Self::physical_lookup_path`] reads it.
    physical_files: HashMap<Ident, PathBuf>,
}

impl ModuleRoots {
    pub fn new(local: Vec<PathBuf>, externs: Vec<ExternRoot>) -> Self {
        let mut registered: IndexMap<Ident, ExternRoot> = IndexMap::new();
        let mut clashes = Vec::new();
        for root in externs {
            match registered.get(&root.name) {
                // Identical file registered twice -- harmless, keep one.
                Some(existing) if existing.file == root.file => {}
                Some(existing) => clashes.push(IdentityClash {
                    name: root.name.clone(),
                    first: existing.file.clone(),
                    second: root.file,
                }),
                None => {
                    registered.insert(root.name.clone(), root);
                }
            }
        }
        Self { local, externs: registered, clashes, physical_files: HashMap::new() }
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
    /// [`ModuleLocation`]), against whichever roots apply to it.
    pub fn locate(&self, path: &[Ident]) -> Result<ModuleLocation, ResolveError> {
        locate_module(self.search_roots_for(path), &self.physical_lookup_path(path))
    }

    /// Whether `path` names a real module on disk at all -- the cheap check
    /// behind "is this an item import or a whole-module import".
    pub fn module_exists(&self, path: &[Ident]) -> bool {
        self.locate(path).is_ok()
    }

    /// Which roots to search for `path`: its own registered extern root if
    /// it's an extern module, else the local project's. Root *selection* is
    /// always keyed by the declared name -- unlike the filename ultimately
    /// opened inside that directory (see [`Self::physical_lookup_path`]),
    /// which an explicit name override does affect.
    fn search_roots_for(&self, path: &[Ident]) -> &[PathBuf] {
        match path.first().and_then(|head| self.externs.get(head)) {
            Some(root) => std::slice::from_ref(&root.dir),
            None => &self.local,
        }
    }

    /// The on-disk segments to actually search for when locating `path` --
    /// identical to `path` unless its root segment's declared identity was
    /// overridden away from its file's real stem (`--name=`/
    /// `--extern=<name>:<file>`), in which case the leading segment is
    /// swapped for that real stem. Segments *after* the root are untouched:
    /// nested module paths are never renamed, only a root's own identity is.
    ///
    /// Critically, the *declared* path -- not this substituted result -- is
    /// what every other piece of driver state keys off of (parsed modules,
    /// sources, mangled symbols, diagnostics, extern-ness). This exists
    /// purely to find the right bytes on disk.
    fn physical_lookup_path(&self, path: &[Ident]) -> ModulePath {
        let physical_stem = path
            .first()
            .and_then(|head| self.physical_files.get(head))
            .and_then(|file| file.file_stem())
            .and_then(|stem| stem.to_str());
        match physical_stem {
            Some(stem) if Some(stem) != path.first().map(Ident::as_ref) => {
                let mut result = Vec::with_capacity(path.len());
                result.push(Ident(stem.to_string()));
                result.extend_from_slice(&path[1..]);
                result
            }
            _ => path.to_vec(),
        }
    }

    /// Records every registered root's declared identity -> real file
    /// mapping (seeding [`Self::physical_lookup_path`]), and reports every
    /// case where two *different* files claim the same identity: two
    /// `--extern`s colliding with each other, or the entry colliding with a
    /// registered extern.
    ///
    /// Must run before any module is located: every lookup translates its
    /// query through `physical_lookup_path`, which reads what this builds.
    pub fn resolve_identities(&mut self, entry: &[Ident], entry_file: &Path) -> Result<(), Vec<CompileError>> {
        let mut errors: Vec<CompileError> = self
            .clashes
            .drain(..)
            .map(|c| CompileError::DuplicateModuleIdentity { name: c.name, first: c.first, second: c.second })
            .collect();

        self.physical_files = self.externs.iter().map(|(name, root)| (name.clone(), root.file.clone())).collect();

        if let Some(head) = entry.first() {
            match self.externs.get(head) {
                Some(existing) if existing.file != entry_file => {
                    errors.push(CompileError::DuplicateModuleIdentity {
                        name: head.clone(),
                        first: entry_file.to_path_buf(),
                        second: existing.file.clone(),
                    });
                }
                _ => {
                    self.physical_files.insert(head.clone(), entry_file.to_path_buf());
                }
            }
        }

        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}
