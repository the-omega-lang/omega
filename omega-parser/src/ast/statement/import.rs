use crate::ast::identifier::Path;

/// `import a::b::c;` -- root-level only (like `extern`/`struct`), never
/// inside a function body: nothing asks for that, and it's easy to add
/// later if it ever comes up. Whether `path` names a whole module or an item
/// inside one isn't decidable from syntax alone (`import a::b::c;` is
/// identical text for both) -- that's resolved later, once the module tree
/// is known, by `omega_analyzer::resolver::ModuleResolver` (implemented by
/// `omega-driver`). The parser only knows this is a path to *something*.
#[derive(Debug, Clone)]
pub struct ImportStmt {
    pub root: ImportRoot,
    pub path: Path,
}

/// Where an `import`'s `path` is anchored -- the leading `root::`/`extern::`
/// the parser peeked for before parsing `path` itself (see
/// `parser::item::parse_item`'s `TokenKind::Import` arm). Purely syntactic;
/// turning this into an actual absolute module path is
/// `omega_driver::Driver::import_absolute_path`'s job, once the module tree
/// is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRoot {
    /// The default: resolved relative to the *importing* module's own
    /// directory (a directory-shaped module's own directory is itself; a
    /// leaf file's is its parent -- see `Driver::relative_base`).
    Local,
    /// `root::...` -- always resolved from the current project's own root,
    /// regardless of how deeply nested the importing module is.
    ProjectRoot,
    /// `extern::name::...` -- resolved from the external project registered
    /// as `name` (via `--extern=name:path`) instead of the local project's
    /// own root; `path.head` is that name, by convention also that
    /// project's own top-level module segment.
    Extern,
}
