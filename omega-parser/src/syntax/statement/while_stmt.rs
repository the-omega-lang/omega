use crate::{parser, prelude::ExpressionNode, syntax::expression::codeblock::CodeblockExpr};
use crate::syntax::statement::StatementNode;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `while cond { ... }` -- a plain statement, not an expression: unlike
/// `if`, a loop's body may run zero or many times, so there's no single
/// "the value it produced" to speak of (this language has no `break
/// <value>` either).
#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub condition: ExpressionNode,
    pub body: CodeblockExpr,
}

impl WhileStmt {
    parser!((expr_parser => ExpressionNode, stmt_parser => StatementNode) => Self {
        text::keyword("while")
            .trivia_padded()
            .ignore_then(expr_parser.clone())
            .then(CodeblockExpr::parser(expr_parser, stmt_parser).trivia_padded())
            .map(|(condition, body)| Self { condition, body })
    });
}
