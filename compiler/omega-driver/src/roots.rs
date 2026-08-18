
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

pub struct ExternRoot {
    pub name: Ident,
    pub dir: PathBuf,
}

pub(crate) struct ModuleRoots {
    local_tree: HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    local_dir: PathBuf,
    externs: IndexMap<Ident, ExternRoot>,
    extern_trees: IndexMap<Ident, HashMap<ModulePath, Result<ModuleLocation, ResolveError>>>,
}

impl ModuleRoots {
    pub fn new(
        local: PathBuf,
        local_name: Option<Ident>,
        externs: Vec<ExternRoot>,
    ) -> Result<Self, Vec<CompileError>> {
        let mut registered: IndexMap<Ident, ExternRoot> = IndexMap::new();
        let mut errors = Vec::new();
        for root in externs {
            match registered.get(&root.name) {
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

    pub fn is_extern(&self, path: &[Ident]) -> bool {
        path.first()
            .is_some_and(|head| self.externs.contains_key(head))
    }

    pub fn has_extern(&self, name: &Ident) -> bool {
        self.externs.contains_key(name)
    }

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

    pub fn module_exists(&self, path: &[Ident]) -> bool {
        self.locate(path).is_ok()
    }

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

    pub fn core_modules(&self) -> Vec<ModulePath> {
        match self.extern_trees.get(&Ident(CORE_MODULE.to_string())) {
            Some(tree) => Self::real_modules(tree),
            None => Self::real_modules(&self.local_tree)
                .into_iter()
                .filter(|path| path.first().map(Ident::as_ref) == Some(CORE_MODULE))
                .collect(),
        }
    }

    pub fn extern_modules(&self) -> Vec<ModulePath> {
        self.extern_trees
            .values()
            .flat_map(Self::real_modules)
            .collect()
    }
}
