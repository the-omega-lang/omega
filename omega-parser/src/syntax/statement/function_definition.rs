use crate::{
    parser,
    prelude::{CodeblockExpr, FunctionType, Statement, StatementNode},
    syntax::{
        ParseError, identifier::Ident, statement::declaration::DeclarationStmt, r#type::Type,
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

impl FunctionDefinitionStmt {
    parser!((stmt_parser => StatementNode) => Self {
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
            .then(CodeblockExpr::parser(stmt_parser).padded())
            .map(
                |(((function_name, params), return_type), codeblock)| FunctionDefinitionStmt {
                    function_name,
                    params,
                    return_type,
                    codeblock,
                },
            )
    });

    pub fn function_type(&self) -> FunctionType {
        let params = self
            .params
            .iter()
            .map(|p| (p.ident.to_owned(), p.r#type.to_owned()))
            .collect::<Vec<_>>();
        FunctionType {
            params,
            return_type: Box::new(self.return_type.clone()),
        }
    }
}
