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
    local_identity: Option<Ident>,
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
        let invalid_identity = |name: &Ident| CompileError::Resolve {
            error: ResolveError::InvalidModuleName {
                path: vec![],
                invalid: name.to_string(),
            },
            importer: None,
        };
        if let Some(name) = &local_name
            && !fs_resolve::is_valid_module_name(name.as_ref())
        {
            errors.push(invalid_identity(name));
        }
        for root in externs {
            if !fs_resolve::is_valid_module_name(root.name.as_ref()) {
                errors.push(invalid_identity(&root.name));
                continue;
            }
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
        let local_identity = local_tree
            .keys()
            .find(|path| path.len() == 1)
            .and_then(|path| path.first())
            .cloned();
        if let Some(identity) = &local_identity
            && let Some(extern_root) = registered.get(identity)
        {
            return Err(vec![CompileError::DuplicateModuleIdentity {
                name: identity.clone(),
                first: local,
                second: extern_root.dir.clone(),
            }]);
        }
        // Unconditional here: `relabel_root` is already a
        // no-op whenever an extern's declared name matches its own
        // on-disk basename (the common case -- `core`, `mathlib`, and
        // every existing extern), and there's no fallback convention to
        // protect for an extern the way there is for the local package.
        let extern_trees: IndexMap<Ident, HashMap<ModulePath, Result<ModuleLocation, ResolveError>>> =
            registered
                .iter()
                .map(|(name, root)| {
                    (
                        name.clone(),
                        fs_resolve::relabel_root(fs_resolve::discover_tree(&root.dir), name),
                    )
                })
                .collect();

        // Fail package discovery clearly, before any semantic compilation
        // begins, rather than only when something happens to import the
        // malformed path. Sorted by module path so the reported order does
        // not depend on the trees' internal HashMap iteration order.
        let mut invalid: Vec<(&ModulePath, CompileError)> = invalid_module_name_errors(&local_tree)
            .chain(extern_trees.values().flat_map(invalid_module_name_errors))
            .collect();
        if !invalid.is_empty() {
            invalid.sort_by_key(|(path, _)| {
                path.iter().map(Ident::as_ref).collect::<Vec<_>>().join("::")
            });
            return Err(invalid.into_iter().map(|(_, error)| error).collect());
        }

        Ok(Self {
            local_tree,
            local_dir: local,
            local_identity,
            externs: registered,
            extern_trees,
        })
    }

    pub fn is_extern(&self, path: &[Ident]) -> bool {
        path.first()
            .is_some_and(|head| self.externs.contains_key(head))
    }

    /// Whether `name` is a known top-level package identity: the local
    /// package or a registered dependency. Unprefixed imports resolve
    /// directly against this namespace, with no relative fallback.
    pub fn is_known_top_level(&self, name: &Ident) -> bool {
        self.local_identity.as_ref() == Some(name) || self.externs.contains_key(name)
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

fn invalid_module_name_errors(
    tree: &HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
) -> impl Iterator<Item = (&ModulePath, CompileError)> {
    tree.iter().filter_map(|(path, result)| match result {
        Err(error @ ResolveError::InvalidModuleName { .. }) => Some((
            path,
            CompileError::Resolve {
                error: error.clone(),
                importer: None,
            },
        )),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "omega_roots_test_{}_{}",
                std::process::id(),
                sequence,
            ));
            std::fs::create_dir_all(&root).expect("create test root");
            Self(root)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &std::path::Path, relative: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().expect("test file has a parent"))
            .expect("create test module parent");
        std::fs::write(path, "").expect("write test module");
    }

    #[test]
    fn a_declared_local_identity_named_root_self_or_super_is_rejected() {
        for reserved in ["root", "self", "super"] {
            let local = TestDir::new();
            write(&local.0, "main.omg");

            let result = ModuleRoots::new(local.0.clone(), Some(Ident(reserved.to_string())), vec![]);
            assert!(
                matches!(
                    result,
                    Err(errors) if errors.iter().any(|e| matches!(
                        e,
                        CompileError::Resolve {
                            error: ResolveError::InvalidModuleName { invalid, .. },
                            ..
                        } if invalid == reserved
                    ))
                ),
                "expected declared local identity `{reserved}` to be rejected"
            );
        }
    }

    #[test]
    fn a_declared_dependency_identity_named_root_self_or_super_is_rejected() {
        for reserved in ["root", "self", "super"] {
            let local = TestDir::new();
            write(&local.0, "main.omg");
            let dependency = TestDir::new();
            write(&dependency.0, "lib.omg");

            let result = ModuleRoots::new(
                local.0.clone(),
                None,
                vec![ExternRoot {
                    name: Ident(reserved.to_string()),
                    dir: dependency.0.clone(),
                }],
            );
            assert!(
                matches!(
                    result,
                    Err(errors) if errors.iter().any(|e| matches!(
                        e,
                        CompileError::Resolve {
                            error: ResolveError::InvalidModuleName { invalid, .. },
                            ..
                        } if invalid == reserved
                    ))
                ),
                "expected declared dependency identity `{reserved}` to be rejected"
            );
        }
    }

    #[test]
    fn an_unprefixed_import_head_is_known_top_level_for_local_and_dependency_identities() {
        let local = TestDir::new();
        write(&local.0, "main.omg");
        let dependency = TestDir::new();
        write(&dependency.0, "lib.omg");

        let roots = ModuleRoots::new(
            local.0.clone(),
            None,
            vec![ExternRoot {
                name: Ident("lib".to_string()),
                dir: dependency.0.clone(),
            }],
        )
        .expect("valid roots");

        let local_name = fs_resolve::basename(&local.0).expect("local basename");
        assert!(roots.is_known_top_level(&local_name));
        assert!(roots.is_known_top_level(&Ident("lib".to_string())));
        assert!(!roots.is_known_top_level(&Ident("unregistered".to_string())));
    }
}
