use crate::ModulePath;
use omega_analyzer::resolver::ResolveError;
use omega_parser::prelude::Ident;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn basename(dir: &Path) -> Option<Ident> {
    dir.file_name()?.to_str().map(|s| Ident(s.to_string()))
}

pub fn relabel_root(
    tree: HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    declared: &Ident,
) -> HashMap<ModulePath, Result<ModuleLocation, ResolveError>> {
    let Some(physical) = tree
        .keys()
        .find(|path| path.len() == 1)
        .map(|path| path[0].clone())
    else {
        return tree;
    };
    if &physical == declared {
        return tree;
    }
    let relabel = |mut path: ModulePath| {
        if path.first() == Some(&physical) {
            path[0] = declared.clone();
        }
        path
    };
    tree.into_iter()
        .map(|(path, location)| {
            let location = location.map_err(|error| match error {
                ResolveError::AmbiguousModule(path) => ResolveError::AmbiguousModule(relabel(path)),
                ResolveError::InvalidModuleName { path, invalid } => {
                    ResolveError::InvalidModuleName {
                        path: relabel(path),
                        invalid,
                    }
                }
                other => other,
            });
            (relabel(path), location)
        })
        .collect()
}

#[derive(Clone)]
pub struct ModuleLocation {
    pub own_file: Option<PathBuf>,
    pub children_dir: Option<PathBuf>,
}

enum SegmentError {
    NotFound,
    Ambiguous,
}

fn resolve_segment(dir: &Path, name: &Ident) -> Result<ModuleLocation, SegmentError> {
    let file_path = dir.join(format!("{}.omg", name.as_ref()));
    let dir_path = dir.join(name.as_ref());
    let is_file = file_path.is_file();
    let is_dir = dir_path.is_dir();

    match (is_file, is_dir) {
        (true, true) => Err(SegmentError::Ambiguous),
        (true, false) => Ok(ModuleLocation {
            own_file: Some(file_path),
            children_dir: None,
        }),
        (false, true) => {
            let own = dir_path.join(format!("{}.omg", name.as_ref()));
            let own_file = own.is_file().then_some(own);
            Ok(ModuleLocation {
                own_file,
                children_dir: Some(dir_path),
            })
        }
        (false, false) => Err(SegmentError::NotFound),
    }
}

/// Whether `name` is legal as a module/package identity: a valid Omega
/// identifier that is not one of the three import-navigation spellings.
/// Those spellings stay usable as ordinary contextual identifiers
/// everywhere else (bindings, fields, item names, ...); they are excluded
/// here only because reusing them as a module identity would make
/// `root::`/`self::`/`super::` ambiguous between navigation and a literal
/// module segment.
pub fn is_valid_module_name(name: &str) -> bool {
    use omega_parser::parser::contextual::{ROOT, SELF, SUPER};
    omega_parser::lexer::is_valid_identifier(name) && ![ROOT, SELF, SUPER].contains(&name)
}

/// Whether `dir`'s subtree contains any `.omg` source, at any depth. Used to
/// decide whether an invalid directory segment name is worth reporting: a
/// directory that leads to no Omega source (e.g. `.git`, a build output
/// directory) is simply irrelevant, not an error.
fn contains_omg_source(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "omg") {
            return true;
        }
        if path.is_dir() && contains_omg_source(&path) {
            return true;
        }
    }
    false
}

pub fn discover_tree(root: &Path) -> HashMap<ModulePath, Result<ModuleLocation, ResolveError>> {
    let mut out = HashMap::new();
    // `omgc` rejects a root with no usable final component up front, so this
    // is unreachable in practice; kept because the function must be total.
    let Some(name) = basename(root) else {
        return out;
    };

    let own_file = root.join(format!("{}.omg", name.as_ref()));
    let same_named_child = root.join(name.as_ref());
    let root_path = vec![name.clone()];
    if own_file.is_file() && same_named_child.is_dir() {
        out.insert(root_path, Err(ResolveError::AmbiguousModule(vec![name])));
        return out;
    }

    out.insert(
        root_path,
        Ok(ModuleLocation {
            own_file: own_file.is_file().then_some(own_file),
            children_dir: Some(root.to_path_buf()),
        }),
    );
    discover_into(root, &mut vec![name.clone()], &mut out, Some(&name));
    out
}

fn discover_into(
    dir: &Path,
    prefix: &mut ModulePath,
    out: &mut HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    skip: Option<&Ident>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut candidates: HashSet<String> = HashSet::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(raw) = file_name.to_str() else {
            continue;
        };
        let candidate = raw.strip_suffix(".omg").unwrap_or(raw);
        candidates.insert(candidate.to_string());
    }

    for candidate in candidates {
        if skip.is_some_and(|s| s.as_ref() == candidate) {
            continue;
        }
        if !is_valid_module_name(&candidate) {
            // A malformed candidate must still be reported if it is
            // source-bearing (a direct `.omg` file, or a directory that
            // leads to `.omg` source at any depth): silently dropping it
            // would compile a different module graph than what is on disk.
            // A malformed name that owns no Omega source at all (`.git`, a
            // build output directory, ...) is simply irrelevant.
            let file_path = dir.join(format!("{candidate}.omg"));
            let dir_path = dir.join(&candidate);
            let is_source_bearing =
                file_path.is_file() || (dir_path.is_dir() && contains_omg_source(&dir_path));
            if is_source_bearing {
                let mut key = prefix.clone();
                key.push(Ident(candidate.clone()));
                out.insert(
                    key,
                    Err(ResolveError::InvalidModuleName {
                        path: prefix.clone(),
                        invalid: candidate,
                    }),
                );
            }
            continue;
        }

        let name = Ident(candidate);
        prefix.push(name.clone());
        match resolve_segment(dir, &name) {
            Ok(location) => {
                let children_dir = location.children_dir.clone();
                out.insert(prefix.clone(), Ok(location));
                if let Some(children_dir) = children_dir {
                    discover_into(&children_dir, prefix, out, Some(&name));
                }
            }
            Err(SegmentError::Ambiguous) => {
                out.insert(
                    prefix.clone(),
                    Err(ResolveError::AmbiguousModule(prefix.clone())),
                );
            }
            // Only a filesystem race (entry vanished after read_dir) could hit this.
            Err(SegmentError::NotFound) => {}
        }
        prefix.pop();
    }
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
                "omega_root_layout_test_{}_{}",
                std::process::id(),
                sequence,
            ));
            std::fs::create_dir_all(&root).expect("create test root");
            Self(root)
        }

        fn name(&self) -> Ident {
            basename(&self.0).expect("test root has a basename")
        }

        fn write(&self, relative: &str) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("test file has a parent"))
                .expect("create test module parent");
            std::fs::write(path, "").expect("write test module");
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn root_file_and_children_share_the_root_module() {
        let root = TestDir::new();
        let name = root.name();
        root.write(&format!("{}.omg", name.as_ref()));
        root.write("foo.omg");

        let tree = discover_tree(&root.0);
        let root_location = tree
            .get(&vec![name.clone()])
            .expect("root module is discovered")
            .as_ref()
            .expect("root module is unambiguous");
        assert!(root_location.own_file.is_some());
        assert!(root_location.children_dir.is_some());
        assert!(tree.contains_key(&vec![name, Ident("foo".to_string())]));
    }

    #[test]
    fn a_root_without_its_own_file_is_a_namespace_for_children() {
        let root = TestDir::new();
        let name = root.name();
        root.write("foo.omg");

        let tree = discover_tree(&root.0);
        let root_location = tree
            .get(&vec![name.clone()])
            .expect("namespace root is discovered")
            .as_ref()
            .expect("namespace root is unambiguous");
        assert!(root_location.own_file.is_none());
        assert!(tree.contains_key(&vec![name, Ident("foo".to_string())]));
    }

    #[test]
    fn a_misnamed_root_file_is_only_a_child_module() {
        let root = TestDir::new();
        let name = root.name();
        root.write("mathlib.omg");

        let tree = discover_tree(&root.0);
        let root_location = tree
            .get(&vec![name.clone()])
            .expect("namespace root is discovered")
            .as_ref()
            .expect("namespace root is unambiguous");
        assert!(root_location.own_file.is_none());
        assert!(tree.contains_key(&vec![name, Ident("mathlib".to_string())]));
    }

    #[test]
    fn nested_directory_modules_keep_their_existing_shape() {
        let root = TestDir::new();
        let name = root.name();
        root.write("foo/foo.omg");
        root.write("foo/bar.omg");

        let tree = discover_tree(&root.0);
        assert!(tree.contains_key(&vec![name.clone(), Ident("foo".to_string())]));
        assert!(tree.contains_key(&vec![
            name,
            Ident("foo".to_string()),
            Ident("bar".to_string()),
        ]));
    }

    #[test]
    fn a_same_named_root_file_and_child_directory_are_ambiguous() {
        let root = TestDir::new();
        let name = root.name();
        root.write(&format!("{}.omg", name.as_ref()));
        std::fs::create_dir(root.0.join(name.as_ref())).expect("create colliding child directory");

        let tree = discover_tree(&root.0);
        assert!(matches!(
            tree.get(&vec![name]).expect("collision is retained"),
            Err(ResolveError::AmbiguousModule(_))
        ));
    }

    #[test]
    fn an_invalid_direct_module_file_is_reported_not_dropped() {
        let root = TestDir::new();
        let name = root.name();
        root.write("foo-bar.omg");

        let tree = discover_tree(&root.0);
        let key = vec![name, Ident("foo-bar".to_string())];
        assert!(matches!(
            tree.get(&key),
            Some(Err(ResolveError::InvalidModuleName { invalid, .. })) if invalid == "foo-bar"
        ));
    }

    #[test]
    fn root_self_and_super_are_invalid_module_names_but_other_contextual_words_are_not() {
        for reserved in ["root", "self", "super"] {
            assert!(!is_valid_module_name(reserved), "{reserved} should be rejected");
        }
        for ordinary in ["mut", "comp", "reveal", "helper", "std"] {
            assert!(is_valid_module_name(ordinary), "{ordinary} should be accepted");
        }
    }

    #[test]
    fn a_source_bearing_file_named_root_self_or_super_is_rejected() {
        for reserved in ["root", "self", "super"] {
            let root = TestDir::new();
            let name = root.name();
            root.write(&format!("{reserved}.omg"));

            let tree = discover_tree(&root.0);
            let key = vec![name, Ident(reserved.to_string())];
            assert!(
                matches!(
                    tree.get(&key),
                    Some(Err(ResolveError::InvalidModuleName { invalid, .. })) if invalid == reserved
                ),
                "expected `{reserved}.omg` to be rejected as a module identity"
            );
        }
    }

    #[test]
    fn a_source_bearing_directory_named_root_self_or_super_is_rejected() {
        for reserved in ["root", "self", "super"] {
            let root = TestDir::new();
            let name = root.name();
            root.write(&format!("{reserved}/child.omg"));

            let tree = discover_tree(&root.0);
            let key = vec![name, Ident(reserved.to_string())];
            assert!(
                matches!(
                    tree.get(&key),
                    Some(Err(ResolveError::InvalidModuleName { invalid, .. })) if invalid == reserved
                ),
                "expected a `{reserved}/` directory to be rejected as a module identity"
            );
        }
    }

    #[test]
    fn an_invalid_source_bearing_ancestor_directory_is_reported() {
        let root = TestDir::new();
        let name = root.name();
        // The `.omg` source is only in a deeper, validly-named descendant;
        // the invalid ancestor segment must still be reported.
        root.write("foo-bar/baz.omg");

        let tree = discover_tree(&root.0);
        let key = vec![name.clone(), Ident("foo-bar".to_string())];
        assert!(matches!(
            tree.get(&key),
            Some(Err(ResolveError::InvalidModuleName { invalid, .. })) if invalid == "foo-bar"
        ));
        // The subtree under an unreachable prefix is not separately walked.
        assert!(!tree.contains_key(&vec![name, Ident("foo-bar".to_string()), Ident("baz".to_string())]));
    }

    #[test]
    fn an_invalid_directory_with_no_omega_source_is_silently_irrelevant() {
        let root = TestDir::new();
        let name = root.name();
        root.write(&format!("{}.omg", name.as_ref()));
        std::fs::create_dir_all(root.0.join(".git").join("objects"))
            .expect("create non-source dot-directory");
        std::fs::write(root.0.join(".git").join("HEAD"), "ref: refs/heads/main")
            .expect("write non-omega file");

        let tree = discover_tree(&root.0);
        assert!(!tree.keys().any(|path| path.iter().any(|i| i.as_ref() == ".git")));
    }

    #[test]
    fn relabeling_keeps_an_honestly_named_root_file() {
        let root = TestDir::new();
        let physical = root.name();
        root.write(&format!("{}.omg", physical.as_ref()));

        let declared = Ident("plat".to_string());
        let tree = relabel_root(discover_tree(&root.0), &declared);
        assert!(tree.contains_key(&vec![declared]));
    }
}
