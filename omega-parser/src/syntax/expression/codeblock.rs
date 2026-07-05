use crate::{
    parser,
    syntax::statement::StatementNode,
};
use crate::prelude::ExpressionNode;
use crate::syntax::ParseError;
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `{ stmt; stmt; ... tail }` -- `tail` is the optional final expression with
/// no trailing `;`, whose value is the block's own value (see
/// `CodeblockExpr::parser`'s doc comment).
#[derive(Debug, Clone)]
pub struct CodeblockExpr {
    pub statements: Vec<StatementNode>,
    pub tail: Option<Box<ExpressionNode>>,
}

impl CodeblockExpr {
    parser!((expr_parser => ExpressionNode, stmt_parser => StatementNode) => Self {
        // Naively parsing "statements, greedily, then an optional tail"
        // doesn't work: `if`/`while`/`for`/a bare `{}` are all valid
        // *statements* on their own (see `Statement`'s `terminal` category,
        // no trailing `;` required), so a trailing `if` *expression* meant
        // as the block's tail would otherwise always be swallowed as just
        // another statement first, silently discarding its value. Instead,
        // at every position, try the tail interpretation *first*: does an
        // expression parse here and get immediately followed by this
        // block's closing `}`? If so, that's the tail, full stop. Only if
        // that fails (more content follows, or nothing here parses as an
        // expression at all) is this position parsed as one ordinary
        // statement, with the same question asked again of what remains.
        // `.rewind()` on the `}` check means it's never actually consumed
        // here -- the outer `then_ignore(just('}')...)` below still does
        // that, once, for real.
        type BlockBody = (Vec<StatementNode>, Option<Box<ExpressionNode>>);
        let body = recursive(|body: Recursive<dyn Parser<'a, &'a str, BlockBody, ParseError<'a>>>| {
            // `just('}')` alone, not `.trivia_padded()`: `expr_parser`, used
            // recursively like this (rather than through the one
            // `.trivia_padded()` wrapping the whole outer grammar), doesn't
            // consume trailing trivia itself -- that's normally left to
            // whatever concrete token comes next also being
            // `.trivia_padded()` (every operator/keyword in this grammar
            // is). So the `}` lookahead needs to tolerate leading trivia
            // itself, or it would reject e.g. `{ a + b }` at the whitespace
            // right before the real `}`.
            let tail_only = expr_parser
                .clone()
                .then_ignore(just('}').trivia_padded().rewind())
                .map(|tail| (Vec::<StatementNode>::new(), Some(Box::new(tail))));

            let one_more_stmt = stmt_parser.clone().then(body).map(|(stmt, (mut stmts, tail))| {
                stmts.insert(0, stmt);
                (stmts, tail)
            });

            let end_of_block = empty().to((Vec::<StatementNode>::new(), None::<Box<ExpressionNode>>));

            choice((tail_only, one_more_stmt, end_of_block))
        });

        just('{')
            .trivia_padded()
            .ignore_then(body)
            .then_ignore(just('}').trivia_padded())
            .map(|(statements, tail)| CodeblockExpr { statements, tail })
    });
}
