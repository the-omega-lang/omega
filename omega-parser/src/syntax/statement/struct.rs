use crate::{
    parser,
    prelude::{DeclarationStmt, FunctionDefinitionStmt},
    syntax::identifier::Ident,
};
use chumsky::{prelude::*, text::ascii::keyword};

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub ident: Ident,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

impl StructStmt {
    parser!((decl_parser => DeclarationStmt, fndef_parser => FunctionDefinitionStmt) => Self {
        let declaration_parser = decl_parser.padded().then_ignore(just(';').padded());
        let declarations_parser = declaration_parser.repeated().collect();
        let functions_parser = fndef_parser.padded().repeated().collect();
        keyword("struct").padded()
            .ignore_then(Ident::parser().padded())
            .then_ignore(just('{').padded())
            .then(declarations_parser)
            .then(functions_parser)
            .then_ignore(just('}').padded())
            .map(|((ident, fields), functions)| Self { ident, fields, functions })
    });
}
