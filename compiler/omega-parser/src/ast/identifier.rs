#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Ident(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExpansionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Origin(pub Option<ExpansionId>);

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

#[derive(Debug, Clone)]
pub struct Path {
    pub head: Ident,
    pub tail: Vec<Ident>,
    pub origin: Origin,
}

// Origin is resolution provenance, not syntax. Structural comparisons of
// paths/types intentionally keep their longstanding text-only meaning.
impl PartialEq for Path {
    fn eq(&self, other: &Self) -> bool {
        self.head == other.head && self.tail == other.tail
    }
}

impl Eq for Path {}

impl std::hash::Hash for Path {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.head.hash(state);
        self.tail.hash(state);
    }
}

impl From<Ident> for Path {
    fn from(ident: Ident) -> Self {
        Self {
            head: ident,
            tail: vec![],
            origin: Origin::default(),
        }
    }
}

impl Path {
    pub fn is_unqualified(&self) -> bool {
        self.tail.is_empty()
    }

    pub fn segments(&self) -> Vec<Ident> {
        std::iter::once(self.head.clone())
            .chain(self.tail.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprPath {
    pub path: Path,
    pub generic_args: Vec<crate::ast::r#type::Type>,
    pub args_at: usize,
    pub qualified_spec: Option<QualifiedSpecPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedSpecPath {
    pub target: crate::ast::r#type::Type,
    pub spec: crate::ast::r#type::Type,
}

impl From<Path> for ExprPath {
    fn from(path: Path) -> Self {
        Self {
            path,
            generic_args: vec![],
            args_at: 0,
            qualified_spec: None,
        }
    }
}

impl From<Ident> for ExprPath {
    fn from(ident: Ident) -> Self {
        Path::from(ident).into()
    }
}

impl ExprPath {
    pub fn plain(&self) -> Option<&Path> {
        self.generic_args.is_empty().then_some(&self.path)
    }
}
