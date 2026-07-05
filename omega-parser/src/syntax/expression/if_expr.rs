use crate::{parser, prelude::ExpressionNode, syntax::expression::codeblock::CodeblockExpr};
use crate::syntax::statement::StatementNode;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `if cond { ... } else if cond { ... } else { ... }` -- a genuine
/// expression (unlike `while`/`for`), whose value is whichever branch's
/// block ran (see `CodeblockExpr`'s tail expression). `branches` holds every
/// `if`/`else if` condition-block pair in source order (the first entry is
/// always the leading `if`); `else_branch` is the trailing `else`, if any.
/// Analysis is what enforces that every branch (and the `else`, if present)
/// resolves to the same type -- the parser only knows the shape.
#[derive(Debug, Clone)]
pub struct IfExpr {
    pub branches: Vec<(ExpressionNode, CodeblockExpr)>,
    pub else_branch: Option<CodeblockExpr>,
}

impl IfExpr {
    parser!((expr_parser => ExpressionNode, stmt_parser => StatementNode) => Self {
        let block = CodeblockExpr::parser(expr_parser.clone(), stmt_parser).trivia_padded();

        let if_head = text::keyword("if")
            .trivia_padded()
            .ignore_then(expr_parser.clone())
            .then(block.clone());

        let else_if_head = text::keyword("else")
            .trivia_padded()
            .ignore_then(text::keyword("if").trivia_padded())
            .ignore_then(expr_parser)
            .then(block.clone());

        let else_tail = text::keyword("else").trivia_padded().ignore_then(block);

        if_head
            .then(else_if_head.repeated().collect::<Vec<_>>())
            .then(else_tail.or_not())
            .map(|((first, rest), else_branch)| {
                let mut branches = vec![first];
                branches.extend(rest);
                Self { branches, else_branch }
            })
    });
}
