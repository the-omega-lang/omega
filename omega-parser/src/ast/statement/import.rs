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
    pub path: Path,
}
