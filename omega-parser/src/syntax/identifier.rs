use crate::parser;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Ident(pub String);

// Helpers for string integration
impl AsRef<str> for Ident {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Helpers for string integration
impl ToString for Ident {
    fn to_string(&self) -> String {
        self.0.clone()
    }
}

// Parser
impl Ident {
    parser!(() => Self {
        text::ascii::ident().map(|s: &str| Ident(s.to_string()))
    });
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

    parser!(() => Self {
        // `::` is its own token, tried as part of one atomic combinator
        // rather than two single colons, the same way `:=` is already
        // disambiguated from a bare `:` elsewhere in this grammar -- no new
        // backtracking risk.
        Ident::parser()
            .then(just("::").trivia_padded().ignore_then(Ident::parser()).repeated().collect::<Vec<_>>())
            .map(|(head, tail)| Self { head, tail })
    });
}
