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
    pub is_member_function: bool,
    pub params: Vec<DeclarationStmt>,
    pub return_type: Type,
    pub codeblock: CodeblockExpr,
}

impl FunctionDefinitionStmt {
    parser!((stmt_parser => StatementNode) => Self {
        let decls_parser = DeclarationStmt::parser()
            .separated_by(just(',').padded())
            .collect::<Vec<_>>()
            .or_not()
            .map(|opt| opt.unwrap_or_default());

        let param_parser = choice((
            text::keyword("self")
                .padded()
                .then(just(',').padded().ignore_then(decls_parser.clone()).or_not())
                .map(|(_, rest)| {
                    (true, rest.unwrap_or_default())
                }),
            decls_parser.map(|decls| (false, decls)),
        ));

        Ident::parser()
            .then_ignore(just('(').padded())
            .then(param_parser)
            .then_ignore(just(')').padded())
            .then_ignore(just("=>").padded())
            .then(Type::parser().padded())
            .then(CodeblockExpr::parser(stmt_parser).padded())
            .map(
                |(((function_name, (is_member_function, params)), return_type), codeblock)| FunctionDefinitionStmt {
                    function_name,
                    is_member_function,
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
            is_variadic: false,
            is_member_function: self.is_member_function,
        }
    }
}
