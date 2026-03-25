use crate::{
    prelude::{CodeblockExpr, Statement},
    syntax::{
        ParseError, SyntaxParser, identifier::Ident, statement::declaration::DeclarationStmt,
        r#type::Type,
    },
};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct FunctionDefinitionStmt {
    pub function_name: Ident,
    pub params: Vec<DeclarationStmt>,
    pub return_type: Type,
    pub codeblock: CodeblockExpr,
}

impl SyntaxParser for FunctionDefinitionStmt {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        // WARN: The following parser causes infinite recursion
        //       Thats because currently there is no distinction
        //       between a root (declaration scope level) statement and a regular statement
        //       The code even only compiles because of .boxed(), which WILL be removed
        //       after the issue is fixed
        Ident::parser()
            .then_ignore(just('(').padded())
            .then(DeclarationStmt::parser().repeated().collect::<Vec<_>>())
            .then_ignore(just(')').padded())
            .then_ignore(just("=>").padded())
            .then(Type::parser().padded())
            .then(CodeblockExpr::parser().padded().boxed())
            .map(
                |(((function_name, params), return_type), codeblock)| FunctionDefinitionStmt {
                    function_name,
                    params,
                    return_type,
                    codeblock,
                },
            )
    }
}
