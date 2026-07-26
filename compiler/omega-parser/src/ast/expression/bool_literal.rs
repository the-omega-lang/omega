/// `true`/`false` -- a bare keyword literal, tried before the general
/// `Path`/identifier case in expression-primary position so the keywords
/// aren't instead parsed as (undefined) variable references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoolExpr(pub bool);
