#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Ident(pub String);

impl AsRef<str> for Ident {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Ident {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `a`, or `a::b::c` -- a (possibly qualified) path to something: a type, a
/// value, or a module. `tail` empty means "just a bare name," the ordinary
/// unqualified case every existing single-`Ident` use site used to handle
/// directly; this is that same case generalized rather than a parallel
/// concept, which is why `Type::Named`/`Expression::Path`/`HirPlaceRoot::Path`
/// all carry a `Path` instead of an `Ident` now -- one shape to resolve,
/// whether or not it turns out to need a cross-module lookup (see
/// `omega_analyzer::resolver`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    pub head: Ident,
    pub tail: Vec<Ident>,
}

impl From<Ident> for Path {
    fn from(ident: Ident) -> Self {
        Self { head: ident, tail: vec![] }
    }
}

impl Path {
    pub fn is_unqualified(&self) -> bool {
        self.tail.is_empty()
    }

    /// All segments, head first -- the shape most resolution logic actually
    /// wants (e.g. building an absolute module path).
    pub fn segments(&self) -> Vec<Ident> {
        std::iter::once(self.head.clone()).chain(self.tail.iter().cloned()).collect()
    }

}

/// A path in *expression* position, possibly carrying explicit generic
/// arguments on exactly one of its segments: `Optional<u32>::Some`,
/// `MyNode<i32>` (as a struct literal's name), `mymodule::List<u8>::new`.
/// Unlike type position -- where `<` can only ever mean generic arguments
/// (see `Type::Generic`) -- expression position must disambiguate against
/// the `<` comparison operator, so the parser only ever commits to this
/// reading speculatively (see `parser::expression::parse_expr_path`); a
/// plain path (the overwhelmingly common case) has `generic_args` empty.
///
/// The arguments always attach to the segment that names the (generic)
/// *type*; whatever follows it (`::Some`, `::new`) is a member looked up
/// inside that type, which resolution validates -- structurally this just
/// records where the `<...>` was written (`args_at`, a 0-based segment
/// index, 0 = `path.head`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprPath {
    pub path: Path,
    pub generic_args: Vec<crate::ast::r#type::Type>,
    /// Only meaningful when `generic_args` is non-empty.
    pub args_at: usize,
}

impl From<Path> for ExprPath {
    fn from(path: Path) -> Self {
        Self { path, generic_args: vec![], args_at: 0 }
    }
}

impl From<Ident> for ExprPath {
    fn from(ident: Ident) -> Self {
        Path::from(ident).into()
    }
}

impl ExprPath {
    /// The plain (no explicit generic arguments) view, when this is one --
    /// what every pre-existing path consumer matches on first, so the
    /// common case stays exactly as cheap and simple as it was before
    /// `ExprPath` existed.
    pub fn plain(&self) -> Option<&Path> {
        self.generic_args.is_empty().then_some(&self.path)
    }
}
