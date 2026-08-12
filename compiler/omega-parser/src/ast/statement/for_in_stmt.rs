use crate::ast::expression::{ExpressionNode, codeblock::CodeblockExpr};
use crate::ast::identifier::Ident;
use crate::ast::r#type::Type;

/// `for <mut>? binding in iterator { ... }` -- the iteration-protocol loop,
/// distinct from `ForStmt`'s classic C-style three-clause form (both start
/// with `for`; `parser::statement::parse_for` disambiguates by lookahead,
/// the same way it already disambiguates a walrus/declaration/expression
/// init clause). `binding`/`mutable` mirror `WalrusStmt`'s own shape --
/// exactly one plain identifier, no destructuring, matching every other
/// binding form this language has. `iterator` keeps its own natural type;
/// what it must resolve to (something implementing `core::iterator::
/// ToIterator<T>`) is entirely an analysis-time concern -- see
/// `Analyzer::analyze_for_in`.
#[derive(Debug, Clone)]
pub struct ForInStmt {
    pub mutable: bool,
    pub binding: Ident,
    /// An optional element type (`for value : u8 in bytes`) selecting a
    /// particular `ToIterator<T>` implementation when more than one exists.
    pub binding_type: Option<Type>,
    pub iterator: ExpressionNode,
    pub body: CodeblockExpr,
}
