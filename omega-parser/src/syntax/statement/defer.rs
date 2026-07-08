use crate::{
    parser,
    prelude::{CodeblockExpr, Expression, ExpressionNode},
    syntax::statement::{Statement, StatementNode},
};
use crate::syntax::trivia::TriviaExt;
use chumsky::{prelude::*, text::ascii::keyword};

/// `defer <statement>;` / `defer { ... }` -- schedules `body` to run when
/// the *enclosing function* exits (see `omega_hir::hir::HirDefer` and
/// `omega_codegen`'s epilogue for how). `body` is a bare `Statement`, not a
/// `StatementNode` -- it has no span of its own; lowering reuses the
/// enclosing `defer` statement's span for it, the same way `ForStmt.init`
/// already does for its own wrapped `Statement`.
#[derive(Debug, Clone)]
pub struct DeferStmt {
    pub body: Box<Statement>,
}

impl DeferStmt {
    parser!((expr_parser => ExpressionNode, stmt_parser => StatementNode) => Self {
        // Tried first, and handled specially rather than by just reusing
        // `stmt_parser` for this case too: a bare `{ ... }` used as a
        // statement only reaches `Statement::Expression` through
        // `StatementNode::parser`'s `nonterminal` fallback today, which
        // requires a *mandatory* trailing `;` (unlike `if`/`while`/`for`,
        // which were explicitly hoisted into the no-`;`-required `terminal`
        // group). Parsing the codeblock directly here -- consuming exactly
        // through its own closing `}` and nothing more -- means
        // `defer { ... }` parses with no trailing `;` needed, matching how
        // every other block-terminated construct in this grammar already
        // behaves, without having to touch the general statement grammar.
        let block_body = CodeblockExpr::parser(expr_parser.clone(), stmt_parser.clone())
            .map_with(|cb, extra| Statement::Expression(ExpressionNode {
                expression: Expression::Codeblock(cb),
                span: extra.span(),
            }));

        keyword("defer").trivia_padded()
            .ignore_then(choice((block_body, stmt_parser.map(|node| node.statement))))
            .map(|body| Self { body: Box::new(body) })
    });
}
