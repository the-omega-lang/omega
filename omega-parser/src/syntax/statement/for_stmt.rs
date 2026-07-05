use crate::{
    parser,
    prelude::ExpressionNode,
    syntax::{
        expression::codeblock::CodeblockExpr,
        statement::{Statement, StatementNode, declaration::DeclarationStmt, walrus::WalrusStmt},
    },
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `for init; cond; post { ... }` -- classic C-style, three semicolon
/// separated clauses (each independently optional, e.g. `for ;; { ... }` is
/// a valid, deliberately infinite loop) followed by the body. Like `while`,
/// this is a plain statement, never an expression.
///
/// `init` reuses exactly the shapes `Statement` already has for
/// declare-and-assign (`Walrus`, `Declaration`(`WithInit`)) or a plain
/// expression -- but parsed *without* consuming a trailing `;` itself, since
/// the `for` loop's own grammar supplies the `;` separators between clauses.
/// `return`/`extern`/`struct` aren't included: none of them make sense as a
/// loop's init clause.
#[derive(Debug, Clone)]
pub struct ForStmt {
    pub init: Option<Statement>,
    pub condition: Option<ExpressionNode>,
    pub post: Option<ExpressionNode>,
    pub body: CodeblockExpr,
}

impl ForStmt {
    parser!((expr_parser => ExpressionNode, stmt_parser => StatementNode) => Self {
        let init_stmt = choice((
            WalrusStmt::parser(expr_parser.clone()).map(Statement::Walrus),
            DeclarationStmt::parser()
                .then(just('=').trivia_padded().ignore_then(expr_parser.clone()).or_not())
                .map(|(decl, init)| match init {
                    Some(value) => Statement::DeclarationWithInit(decl, value),
                    None => Statement::Declaration(decl),
                }),
            expr_parser.clone().map(Statement::Expression),
        ));

        // The post clause sits directly before the mandatory body `{...}`,
        // with no separating `;` -- and a bare `{...}`/`if` is itself a
        // valid expression (a `Codeblock`/`If`), so an *empty* post clause
        // would otherwise be ambiguous with "the post clause is the loop's
        // own body." A zero-width, non-consuming lookahead (`.rewind()`)
        // resolves it: if the next token is `{`, there's no post
        // expression, full stop -- it can only be the body.
        let post = just('{').trivia_padded().rewind().to(None)
            .or(expr_parser.clone().map(Some));

        text::keyword("for")
            .trivia_padded()
            .ignore_then(init_stmt.or_not())
            .then_ignore(just(';').trivia_padded())
            .then(expr_parser.clone().or_not())
            .then_ignore(just(';').trivia_padded())
            .then(post)
            .then(CodeblockExpr::parser(expr_parser, stmt_parser).trivia_padded())
            .map(|(((init, condition), post), body)| Self { init, condition, post, body })
    });
}
