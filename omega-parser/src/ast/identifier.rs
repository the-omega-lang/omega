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
