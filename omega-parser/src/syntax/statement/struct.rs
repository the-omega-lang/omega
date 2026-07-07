use crate::{
    parser,
    prelude::{DeclarationStmt, FunctionDefinitionStmt},
    syntax::identifier::Ident,
};
use crate::syntax::trivia::TriviaExt;
use chumsky::{prelude::*, text::ascii::keyword};

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub ident: Ident,
    /// `<T, U, ...>` immediately after `ident` -- empty for an ordinary,
    /// non-generic struct. See `Type::Generic`'s doc comment for how these
    /// names are referenced at a use site.
    pub generics: Vec<Ident>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

impl StructStmt {
    parser!((decl_parser => DeclarationStmt, fndef_parser => FunctionDefinitionStmt) => Self {
        let declaration_parser = decl_parser.trivia_padded().then_ignore(just(';').trivia_padded());
        let declarations_parser = declaration_parser.repeated().collect();
        let functions_parser = fndef_parser.trivia_padded().repeated().collect();
        let generics_parser = just('<').trivia_padded()
            .ignore_then(Ident::parser().separated_by(just(',').trivia_padded()).at_least(1).collect::<Vec<_>>())
            .then_ignore(just('>').trivia_padded())
            .or_not()
            .map(|opt| opt.unwrap_or_default());
        keyword("struct").trivia_padded()
            .ignore_then(Ident::parser().trivia_padded())
            .then(generics_parser)
            .then_ignore(just('{').trivia_padded())
            .then(declarations_parser)
            .then(functions_parser)
            .then_ignore(just('}').trivia_padded())
            .map(|(((ident, generics), fields), functions)| Self { ident, generics, fields, functions })
    });
}
