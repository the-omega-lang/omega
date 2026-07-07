use crate::{
    parser,
    syntax::{
        identifier::Ident,
        token::{self, Token},
    },
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// `name!(arg, ...)` -- a macro invocation. Shared verbatim between
/// expression position (`Expression::MacroInvocation`, usable anywhere an
/// expression can appear) and module-top-level item position
/// (`RootStatement::MacroInvocation`, for an `items`-output macro) rather
/// than duplicated into two near-identical types, since the grammar and
/// payload shape are identical either way -- only *where* the parser is
/// wired in differs. Each argument is kept as a raw [`Token`] list (not
/// parsed as an `Expression`/`Type` here) since a `Type`-fragment argument
/// (e.g. `generate_type!(Counter)`) isn't valid expression syntax; see
/// `omega_parser::macros` for where each argument is validated against its
/// parameter's declared `FragmentKind` and substituted.
#[derive(Debug, Clone)]
pub struct MacroInvocationExpr {
    pub name: Ident,
    pub args: Vec<Vec<Token>>,
}

impl MacroInvocationExpr {
    parser!(() => Self {
        // `!`'s own `.trivia_padded()` (rather than requiring bare
        // adjacency to `name`) matters beyond just permissiveness: a macro
        // invocation appearing *inside* another macro's substituted body
        // has already gone through `token::render`, which always joins
        // tokens with a single space (see that function's doc comment) --
        // so the rendered text is always `name ! (...)`, never `name!(...)`.
        Ident::parser()
            .then_ignore(just('!').trivia_padded())
            .then(token::args_parser())
            .map(|(name, args)| Self { name, args })
            .trivia_padded()
    });
}
