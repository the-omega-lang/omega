use crate::ModulePath;
use omega_analyzer::resolver::ResolveError;
use omega_parser::prelude::Ident;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A root directory's own bare, on-disk identity: its basename. This is
/// only ever the *default* declared identity of a project root -- both the
/// local package's own root and every `--extern` target may instead be
/// given an explicit override (`--name=`/`--extern=<name>:<dir>`) that
/// differs from it, in which case `relabel_root` is what actually makes
/// that override apply beneath the root, not just at it. `None` for a path
/// with no usable final component (`/`, `.`, `..`, or one that isn't valid
/// UTF-8).
pub fn basename(dir: &Path) -> Option<Ident> {
    dir.file_name()?.to_str().map(|s| Ident(s.to_string()))
}

/// Rewrites every path in a freshly discovered tree so it's addressed by
/// `declared` instead of whatever the root's real on-disk name turned out
/// to be -- the same translation `--name=`/`--extern=<name>:<dir>` already
/// apply to the root itself, extended to everything nested beneath it, so
/// a directory honestly named `libc` can still present as the package
/// `plat` in full, not just at its own entry. A no-op (returns `tree`
/// unchanged) when the tree is empty/malformed (no single root segment to
/// relabel) or already matches `declared`.
pub fn relabel_root(
    tree: HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    declared: &Ident,
) -> HashMap<ModulePath, Result<ModuleLocation, ResolveError>> {
    let Some(physical) = tree.keys().find(|path| path.len() == 1).map(|path| path[0].clone()) else {
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
                other => other,
            });
            (relabel(path), location)
        })
        .collect()
}

/// Where a module's own content (if any) and further children (if any) live
/// on disk. See the module-tree discovery rule this crate implements: a
/// bare `name.omg` file is a leaf (`children_dir: None`); a directory
/// `name/` is a module whose own items come from `name/name.omg` if that
/// file exists (`own_file: Some`) or nowhere at all (a namespace-only
/// module, `own_file: None`) -- either way its children live in `name/`
/// (`children_dir: Some`).
#[derive(Clone)]
pub struct ModuleLocation {
    pub own_file: Option<PathBuf>,
    pub children_dir: Option<PathBuf>,
}

enum SegmentError {
    NotFound,
    /// Both `dir/name.omg` and `dir/name/` exist -- ambiguous, deliberately
    /// not resolved by an implicit tie-break rule.
    Ambiguous,
}

/// Resolves one path segment (`name`) directly inside `dir` -- no
/// recursion; `discover_into` is the only caller, walking one path
/// component of the tree at a time.
fn resolve_segment(dir: &Path, name: &Ident) -> Result<ModuleLocation, SegmentError> {
    let file_path = dir.join(format!("{}.omg", name.as_ref()));
    let dir_path = dir.join(name.as_ref());
    let is_file = file_path.is_file();
    let is_dir = dir_path.is_dir();

    match (is_file, is_dir) {
        (true, true) => Err(SegmentError::Ambiguous),
        (true, false) => Ok(ModuleLocation { own_file: Some(file_path), children_dir: None }),
        (false, true) => {
            let own = dir_path.join(format!("{}.omg", name.as_ref()));
            let own_file = own.is_file().then_some(own);
            Ok(ModuleLocation { own_file, children_dir: Some(dir_path) })
        }
        (false, false) => Err(SegmentError::NotFound),
    }
}

/// A very small mirror of the lexer's own identifier grammar
/// (`omega_parser::lexer`'s private `is_ident_start`/`is_ident_continue`) --
/// enough to tell a real module segment apart from a dotfile or anything
/// else that could never be a valid `Ident` in the first place, without
/// this crate depending on lexer internals for it.
fn is_module_segment_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Recursively discovers every module reachable under `root` -- the eager
/// inventory backing `ModuleRoots::locate`, for the local package's own
/// root and for every registered `--extern`'s root alike (`ModuleRoots::
/// new` calls this for both). Reuses `resolve_segment` for each name
/// actually found on disk, so the file-vs-directory decision is made in
/// exactly one place regardless of whether a path was asked about or
/// discovered. Metadata-only -- nothing here ever opens a file, only
/// `read_dir`/`is_file`/`is_dir`, so this stays cheap no matter how large
/// the tree is.
///
/// A directory entry whose name isn't a syntactically valid module segment
/// (a dotfile, anything with characters no `Ident` can have) is silently
/// skipped -- it could never be resolved by an on-demand lookup either, so
/// walking into it (`.git/`, say) would only cost time for zero benefit.
///
/// An ambiguous segment (both `name.omg` and `name/` present) is recorded
/// as `Err(AmbiguousModule(..))` rather than silently dropped, so a later
/// lookup against this inventory reports the real diagnostic instead of a
/// misleading "doesn't exist".
pub fn discover_tree(root: &Path) -> HashMap<ModulePath, Result<ModuleLocation, ResolveError>> {
    let mut out = HashMap::new();
    // Unreachable in practice: `omgc` rejects a root with no usable final
    // component (`.`, `/`, `..`) up front, for the local package and every
    // `--extern` alike, precisely so this can never become a *second*,
    // silent way to end up with an empty tree and an empty object file. The
    // empty map stays here only because this function is total; it is not a
    // supported outcome.
    let Some(name) = basename(root) else {
        return out;
    };

    // A package root follows the same directory-shaped-module convention as
    // every nested directory: `<root>/<basename>.omg` is its own content and
    // the root directory contains its children.  A same-named child directory
    // would claim the identical path, so preserve the ordinary ambiguity
    // diagnostic rather than inventing a root-only tie break.
    let own_file = root.join(format!("{}.omg", name.as_ref()));
    let same_named_child = root.join(name.as_ref());
    let root_path = vec![name.clone()];
    if own_file.is_file() && same_named_child.is_dir() {
        out.insert(
            root_path,
            Err(ResolveError::AmbiguousModule(vec![name])),
        );
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

/// `skip`: the one entry name to ignore in `dir`, when `dir` is itself a
/// directory-shaped module's own `children_dir` -- that directory's
/// `<name>.omg` is exactly the `own_file` its *parent* call already
/// recorded one level up (at `prefix` before `name` was pushed onto it,
/// see the recursive call below), not a fresh sibling. Without this, a
/// directory-shaped module named the same as its own entry file
/// (`X/X.omg`) gets double-counted: this same rescan would find `X.omg`
/// again and register a spurious `[...prefix, "X"]` pointing at the
/// identical file `[...prefix]` already has. `None` only at the very top
/// after `discover_tree` has registered the root module itself.
fn discover_into(
    dir: &Path,
    prefix: &mut ModulePath,
    out: &mut HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
    skip: Option<&Ident>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: HashSet<Ident> = HashSet::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(raw) = file_name.to_str() else { continue };
        let candidate = raw.strip_suffix(".omg").unwrap_or(raw);
        if is_module_segment_name(candidate) {
            names.insert(Ident(candidate.to_string()));
        }
    }

    for name in names {
        if skip.is_some_and(|s| s == &name) {
            continue;
        }
        prefix.push(name.clone());
        match resolve_segment(dir, &name) {
            Ok(location) => {
                let children_dir = location.children_dir.clone();
                out.insert(prefix.clone(), Ok(location));
                if let Some(children_dir) = children_dir {
                    discover_into(&children_dir, prefix, out, Some(&name));
                }
            }
            // A real, on-disk collision (both `name.omg` and `name/`
            // exist) -- kept, not dropped, so a lookup against this
            // inventory reports it instead of a misleading "not found".
            Err(SegmentError::Ambiguous) => {
                out.insert(prefix.clone(), Err(ResolveError::AmbiguousModule(prefix.clone())));
            }
            // Can only happen from a filesystem race (removed between
            // `read_dir` and this check) -- `name` was just observed to
            // exist, so this is never a real "doesn't exist" case.
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
    fn relabeling_keeps_an_honestly_named_root_file() {
        let root = TestDir::new();
        let physical = root.name();
        root.write(&format!("{}.omg", physical.as_ref()));

        let declared = Ident("plat".to_string());
        let tree = relabel_root(discover_tree(&root.0), &declared);
        assert!(tree.contains_key(&vec![declared]));
    }
}
