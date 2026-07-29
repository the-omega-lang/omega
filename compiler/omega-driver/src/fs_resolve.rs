use crate::ModulePath;
use omega_analyzer::resolver::ResolveError;
use omega_parser::prelude::Ident;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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

/// Resolves an absolute *on-disk* module path (e.g. `["mymodule", "thing"]`)
/// against every search root, first match wins. Callers reach this through
/// [`crate::roots::ModuleRoots`], which decides which roots apply and
/// translates a declared name into its real on-disk stem first -- nothing
/// here knows anything about declared identity, externs, or overrides.
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
/// inventory backing `ModuleRoots::locate` for whichever package is
/// actually *being compiled* (never for an `--extern` dependency, which
/// stays purely on-demand via `locate_module` above -- see the module-level
/// design this implements). Reuses `resolve_segment` for each name actually
/// found on disk, so the file-vs-directory decision is made in exactly one
/// place regardless of whether a path was asked about or discovered.
/// Metadata-only -- nothing here ever opens a file, only `read_dir`/
/// `is_file`/`is_dir`, so this stays cheap no matter how large the tree is.
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
    discover_into(root, &mut Vec::new(), &mut out);
    out
}

fn discover_into(
    dir: &Path,
    prefix: &mut ModulePath,
    out: &mut HashMap<ModulePath, Result<ModuleLocation, ResolveError>>,
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
        prefix.push(name.clone());
        match resolve_segment(dir, &name) {
            Ok(location) => {
                let children_dir = location.children_dir.clone();
                out.insert(prefix.clone(), Ok(location));
                if let Some(children_dir) = children_dir {
                    discover_into(&children_dir, prefix, out);
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
