use crate::{
    parser,
    prelude::{ExpressionNode, Ident},
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `ident := value;` -- "declare and assign", with `ident`'s type inferred
/// from `value`'s resolved type rather than written out explicitly like
/// `DeclarationStmt`. Function-body statements only (not a `RootStatement`):
/// a top-level `x := 5;` would hit the exact same "global data declarations
/// are not yet implemented" gap a top-level `x : i32;` already hits, so
/// there's no capability to gain from supporting it there yet.
#[derive(Debug, Clone)]
pub struct WalrusStmt {
    pub ident: Ident,
    pub value: ExpressionNode,
}

impl WalrusStmt {
    parser!((expr_parser => ExpressionNode) => Self {
        Ident::parser()
            .trivia_padded()
            .then_ignore(just(":=").trivia_padded())
            .then(expr_parser)
            .map(|(ident, value)| Self { ident, value })
    });
}
