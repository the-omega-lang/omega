/// `exposed`/`internal`/(no modifier) on an item or field -- see
/// `parser::item::parse_optional_visibility` for the contextual-keyword
/// grammar (same "stays a plain `Ident`, recognized by text" philosophy as
/// `mut`, see `lexer::TokenKind`'s doc comment) and
/// `omega_analyzer::analysis::Analyzer::check_visibility` for how each
/// variant is actually enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Visibility {
    /// The default when no modifier is written -- visible only within the
    /// exact module that declares it (not submodules, not siblings).
    #[default]
    Private,
    /// `internal` -- visible anywhere within the same top-level package
    /// (same root module segment), regardless of nesting depth or
    /// ancestor/descendant relationship. Rust `pub(crate)`-style.
    Internal,
    /// `exposed` -- visible from anywhere, no restriction.
    Exposed,
}
