use omega_analyzer::resolver::ResolveError;
use omega_parser::prelude::Ident;
use std::path::{Path, PathBuf};

/// Where a module's own content (if any) and further children (if any) live
/// on disk. See the module-tree discovery rule this crate implements: a
/// bare `name.omg` file is a leaf (`children_dir: None`); a directory
/// `name/` is a module whose own items come from `name/name.omg` if that
/// file exists (`own_file: Some`) or nowhere at all (a namespace-only
/// module, `own_file: None`) -- either way its children live in `name/`
/// (`children_dir: Some`).
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

/// Resolves one path segment (`name`) directly inside `dir` -- no recursion,
/// no search-root fallback (see `locate_module` for that).
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

/// Walks `path` segment by segment starting at `root`, descending into each
/// segment's `children_dir` in turn -- a path can only continue past a
/// segment that turned out to be a directory-shaped module (a bare leaf
/// file, by definition, has no children to descend into).
fn locate_from(root: &Path, path: &[Ident]) -> Result<ModuleLocation, SegmentError> {
    let mut current_dir = root.to_path_buf();
    let mut result = Err(SegmentError::NotFound);

    for (i, segment) in path.iter().enumerate() {
        let location = resolve_segment(&current_dir, segment)?;
        if i == path.len() - 1 {
            result = Ok(location);
            break;
        }
        current_dir = location.children_dir.ok_or(SegmentError::NotFound)?;
    }

    result
}

/// Resolves an absolute module path (e.g. `["mymodule", "thing"]`) against
/// every search root, first match wins -- the one place this crate's
/// "multiple include paths for future package support" requirement is
/// implemented; today `roots` always has exactly one entry, but nothing
/// above this function needs to change to add more.
pub fn locate_module(roots: &[PathBuf], path: &[Ident]) -> Result<ModuleLocation, ResolveError> {
    let mut ambiguous = false;
    for root in roots {
        match locate_from(root, path) {
            Ok(location) => return Ok(location),
            Err(SegmentError::Ambiguous) => ambiguous = true,
            Err(SegmentError::NotFound) => {}
        }
    }

    if ambiguous {
        Err(ResolveError::AmbiguousModule(path.to_vec()))
    } else {
        Err(ResolveError::UnknownModule(path.to_vec()))
    }
}
