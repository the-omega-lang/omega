use crate::ast::annotation::AnnotationNode;
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
    /// `@suppress(...)` written directly above `import` -- the only
    /// annotation an import accepts (see `omega_analyzer::annotations::
    /// ItemKind::Import`); anything else is rejected the ordinary
    /// `AnnotationNotApplicable` way.
    pub annotations: Vec<AnnotationNode>,
    /// `import hidden path;` -- bypasses the visibility check on whatever
    /// `path` resolves to, for *this importing module's own* later
    /// references through the resulting alias (does not make the alias
    /// itself visible to any third module -- there is no re-export concept
    /// in this language). See `omega_analyzer::analysis::Analyzer::
    /// hidden_stack`'s doc comment for the general `hidden` mechanism this
    /// plugs into.
    pub hidden: bool,
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
