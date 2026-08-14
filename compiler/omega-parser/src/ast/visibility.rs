/// `exposed`/`internal`/(no modifier) on an item or field -- see
/// `parser::item::parse_optional_visibility` for the contextual-keyword
/// grammar (same "stays a plain `Ident`, recognized by text" philosophy as
/// `mut`, see `lexer::TokenKind`'s doc comment) and
/// `omega_analyzer::analysis::Analyzer::check_visibility` for how each
/// variant is actually enforced.
///
/// Declared least to most permissive, and `PartialOrd`/`Ord`-derived on
/// that basis: `Hidden < Internal < Exposed`. This ordering is itself
/// meaningful, not just a derive of convenience -- a spec implementation's
/// own visibility must be `>=` the spec function it's satisfying (see
/// conformance checking), which
/// is exactly `own_visibility >= required_visibility` on this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Visibility {
    /// The default when no modifier is written -- visible only within the
    /// exact module that declares it (not submodules, not siblings).
    #[default]
    Hidden,
    /// `internal` -- visible anywhere within the same top-level package
    /// (same root module segment), regardless of nesting depth or
    /// ancestor/descendant relationship. Rust `pub(crate)`-style.
    Internal,
    /// `exposed` -- visible from anywhere, no restriction.
    Exposed,
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Visibility::Hidden => "hidden",
            Visibility::Internal => "internal",
            Visibility::Exposed => "exposed",
        })
    }
}
