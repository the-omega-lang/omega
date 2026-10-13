use crate::error::CompileError;
pub(crate) const CORE_MODULE: &str = "core";

pub(crate) fn is_core_module(path: &ModulePath) -> bool {
    path.root().map(Ident::as_ref) == Some(CORE_MODULE)
}
use crate::fs_resolve::{self, ModuleLocation};
use indexmap::IndexMap;
use omega_analyzer::resolver::ResolveError;
use omega_hir::ModulePath;
use omega_parser::prelude::Ident;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct ExternRoot {
    pub name: Ident,
    pub dir: PathBuf,
}

/// One physical, source-bearing local `.omg` file. `relative_path` is the
/// file's path below the local package root and is the compiler's stable
/// native emission identity: it mirrors on-disk layout, so a declared
/// `<name>=<dir>` identity override never moves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSource {
    pub module: ModulePath,
    pub relative_path: PathBuf,
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
                path: ModulePath::new([]),
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
            .and_then(|path| path.root())
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
        let core = Ident(CORE_MODULE.to_string());
        if local_identity.as_ref() != Some(&core) && !registered.contains_key(&core) {
            return Err(vec![CompileError::MissingCore]);
        }
        // Unconditional here: `relabel_root` is already a
        // no-op whenever an extern's declared name matches its own
        // on-disk basename (the common case -- `core`, `mathlib`, and
        // every existing extern), and there's no fallback convention to
        // protect for an extern the way there is for the local package.
        let extern_trees: IndexMap<
            Ident,
            HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
        > = registered
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
            invalid.sort_by_key(|(path, _)| path.to_string());
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

    pub fn is_extern(&self, path: &ModulePath) -> bool {
        path.root()
            .is_some_and(|head| self.externs.contains_key(head))
    }

    /// Whether `name` is a known top-level package identity: the local
    /// package or a registered dependency. Unprefixed imports resolve
    /// directly against this namespace, with no relative fallback.
    pub fn is_known_top_level(&self, name: &Ident) -> bool {
        self.local_identity.as_ref() == Some(name) || self.externs.contains_key(name)
    }

    pub fn locate(&self, path: &ModulePath) -> Result<ModuleLocation, ResolveError> {
        let tree = if self.is_extern(path) {
            &self.extern_trees[path.root().expect("an extern path names its package")]
        } else {
            &self.local_tree
        };
        match tree.get(path) {
            Some(result) => result.clone(),
            None => Err(ResolveError::UnknownModule(path.clone())),
        }
    }

    pub fn module_exists(&self, path: &ModulePath) -> bool {
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

    pub fn source_modules(&self) -> Vec<ModulePath> {
        let mut paths = Self::real_modules(&self.local_tree);
        paths.extend(self.extern_modules());
        paths.sort();
        paths
    }

    pub fn prune(&mut self, disabled: &[ModulePath]) {
        let keep = |path: &ModulePath, _: &mut Result<ModuleLocation, ResolveError>| {
            !disabled.iter().any(|parent| path.starts_with(parent))
        };
        self.local_tree.retain(keep);
        for tree in self.extern_trees.values_mut() {
            tree.retain(keep);
        }
        self.local_identity = self
            .local_tree
            .keys()
            .find(|path| path.len() == 1)
            .and_then(|path| path.root())
            .cloned();
    }

    /// Every source-bearing local `.omg` file, sorted by its path relative to
    /// the package root. A namespace-only directory bears no source of its
    /// own and therefore has no entry.
    pub fn local_sources(&self) -> Vec<LocalSource> {
        let mut sources: Vec<LocalSource> = self
            .local_tree
            .iter()
            .filter_map(|(module, result)| {
                let file = result.as_ref().ok()?.own_file.as_ref()?;
                Some(LocalSource {
                    module: module.clone(),
                    relative_path: relative_source_path(&self.local_dir, file),
                })
            })
            .collect();
        sources.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        sources
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
                .filter(is_core_module)
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

/// `discover_tree` only ever builds locations below the package root, so the
/// fallback is unreachable; it keeps a caller from ever joining an absolute
/// path under an output directory.
fn relative_source_path(root: &Path, file: &Path) -> PathBuf {
    match file.strip_prefix(root) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => PathBuf::from(file.file_name().unwrap_or(file.as_os_str())),
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

    /// An empty `core` package, which every compilation but `core`'s own needs.
    fn bare_core(dir: &TestDir) -> ExternRoot {
        write(&dir.0, "core.omg");
        ExternRoot {
            name: Ident(CORE_MODULE.into()),
            dir: dir.0.clone(),
        }
    }

    fn write(dir: &std::path::Path, relative: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().expect("test file has a parent"))
            .expect("create test module parent");
        std::fs::write(path, "").expect("write test module");
    }

    #[test]
    fn pruning_removes_subtrees_and_recomputes_local_identity() {
        let local = TestDir::new();
        let external = TestDir::new();
        write(&local.0, "thing/thing.omg");
        write(&local.0, "thing/child.omg");
        write(&local.0, "keep.omg");
        write(&external.0, "thing/thing.omg");
        write(&external.0, "thing/child.omg");
        let name = Ident("local".into());
        let ext = Ident("ext".into());
        let core = TestDir::new();
        let mut roots = ModuleRoots::new(
            local.0.clone(),
            Some(name.clone()),
            vec![
                ExternRoot {
                    name: ext.clone(),
                    dir: external.0.clone(),
                },
                bare_core(&core),
            ],
        )
        .unwrap();
        roots.prune(&[
            ModulePath::new([name.clone(), Ident("thing".into())]),
            ModulePath::new([ext.clone()]),
            ModulePath::new([Ident(CORE_MODULE.into())]),
        ]);
        assert_eq!(roots.local_sources().len(), 1);
        assert!(roots.extern_modules().is_empty());
        assert!(matches!(
            roots.locate(&ModulePath::new([
                name.clone(),
                Ident("thing".into()),
                Ident("child".into())
            ])),
            Err(ResolveError::UnknownModule(_))
        ));
        assert!(roots.is_known_top_level(&name));
        roots.prune(&[ModulePath::new([name.clone()])]);
        assert!(!roots.is_known_top_level(&name));
        assert!(roots.local_sources().is_empty());
    }

    #[test]
    fn a_declared_local_identity_named_root_self_or_super_is_rejected() {
        for reserved in ["root", "self", "super"] {
            let local = TestDir::new();
            write(&local.0, "main.omg");

            let result =
                ModuleRoots::new(local.0.clone(), Some(Ident(reserved.to_string())), vec![]);
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

    fn sources(root: &TestDir, declared: Option<&str>) -> Vec<LocalSource> {
        let core = TestDir::new();
        ModuleRoots::new(
            root.0.clone(),
            declared.map(|name| Ident(name.to_string())),
            vec![bare_core(&core)],
        )
        .expect("valid roots")
        .local_sources()
    }

    fn relative_paths(sources: &[LocalSource]) -> Vec<String> {
        sources
            .iter()
            .map(|source| source.relative_path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn every_source_bearing_local_file_gets_one_sorted_relative_emission_identity() {
        let local = TestDir::new();
        let name = fs_resolve::basename(&local.0).expect("test root basename");
        write(&local.0, &format!("{}.omg", name.as_ref()));
        write(&local.0, "child.omg");
        write(&local.0, "sub/sub.omg");
        write(&local.0, "sub/leaf.omg");
        // A namespace-only directory: children but no own file.
        write(&local.0, "space/inner.omg");

        let sources = sources(&local, None);
        let mut expected = vec![
            format!("{}.omg", name.as_ref()),
            "child.omg".to_string(),
            "space/inner.omg".to_string(),
            "sub/leaf.omg".to_string(),
            "sub/sub.omg".to_string(),
        ];
        expected.sort();
        assert_eq!(relative_paths(&sources), expected);
        assert!(
            !sources.iter().any(|source| source.module
                == ModulePath::new([name.clone(), Ident("space".to_string())])),
            "a namespace-only directory owns no source and must have no emission identity"
        );
        let root_source = sources
            .iter()
            .find(|source| source.module == ModulePath::new([name.clone()]))
            .expect("the root module is source-bearing");
        assert_eq!(
            root_source.relative_path,
            PathBuf::from(format!("{}.omg", name.as_ref()))
        );
    }

    #[test]
    fn an_empty_source_still_owns_an_emission_identity() {
        let local = TestDir::new();
        let name = fs_resolve::basename(&local.0).expect("test root basename");
        write(&local.0, &format!("{}.omg", name.as_ref()));
        write(&local.0, "silent.omg");

        assert!(
            relative_paths(&sources(&local, None)).contains(&"silent.omg".to_string()),
            "a source with no items still owns its artifact"
        );
    }

    #[test]
    fn a_declared_package_identity_renames_modules_but_not_source_paths() {
        let local = TestDir::new();
        let physical = fs_resolve::basename(&local.0).expect("test root basename");
        write(&local.0, &format!("{}.omg", physical.as_ref()));
        write(&local.0, "child.omg");

        let sources = sources(&local, Some("renamed"));
        let mut expected = vec![
            format!("{}.omg", physical.as_ref()),
            "child.omg".to_string(),
        ];
        expected.sort();
        assert_eq!(relative_paths(&sources), expected);
        assert!(
            sources.iter().all(|source| source
                .module
                .root()
                .is_some_and(|root| root.as_ref() == "renamed")),
            "declared identity renames the logical module path"
        );
    }

    #[test]
    fn an_unprefixed_import_head_is_known_top_level_for_local_and_dependency_identities() {
        let local = TestDir::new();
        write(&local.0, "main.omg");
        let dependency = TestDir::new();
        write(&dependency.0, "lib.omg");

        let core = TestDir::new();
        let roots = ModuleRoots::new(
            local.0.clone(),
            None,
            vec![
                ExternRoot {
                    name: Ident("lib".to_string()),
                    dir: dependency.0.clone(),
                },
                bare_core(&core),
            ],
        )
        .expect("valid roots");

        let local_name = fs_resolve::basename(&local.0).expect("local basename");
        assert!(roots.is_known_top_level(&local_name));
        assert!(roots.is_known_top_level(&Ident("lib".to_string())));
        assert!(!roots.is_known_top_level(&Ident("unregistered".to_string())));
    }

    #[test]
    fn a_compilation_without_core_is_rejected() {
        let local = TestDir::new();
        write(&local.0, "main.omg");
        let result = ModuleRoots::new(local.0.clone(), Some(Ident("main".into())), vec![]);
        assert!(
            matches!(&result, Err(errors) if matches!(errors.as_slice(), [CompileError::MissingCore])),
            "a package other than core must register it"
        );
    }

    #[test]
    fn core_itself_needs_no_core_extern() {
        let local = TestDir::new();
        write(&local.0, "core.omg");
        ModuleRoots::new(local.0.clone(), Some(Ident(CORE_MODULE.into())), vec![])
            .expect("core is the package being compiled");
    }
}
