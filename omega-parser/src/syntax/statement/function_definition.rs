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
        Ident::parser()
            .then_ignore(just('(').padded())
            .then(
                DeclarationStmt::parser()
                    .separated_by(just(','))
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(')').padded())
            .then_ignore(just("=>").padded())
            .then(Type::parser().padded())
            .then(CodeblockExpr::parser().padded())
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
