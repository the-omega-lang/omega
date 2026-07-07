use crate::{
    parser,
    prelude::{CodeblockExpr, ExpressionNode, FunctionType, StatementNode},
    syntax::{
        identifier::Ident, statement::declaration::DeclarationStmt, r#type::Type,
    },
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub struct FunctionDefinitionStmt {
    pub function_name: Ident,
    /// `<T, U, ...>` immediately after `function_name` -- empty for an
    /// ordinary, non-generic function. Unlike a struct's, these are never
    /// referenced with explicit arguments at a call site: they're deduced
    /// from the call's own argument types (see `Analyzer::resolve_generic_call`).
    pub generics: Vec<Ident>,
    pub is_member_function: bool,
    pub params: Vec<DeclarationStmt>,
    pub return_type: Type,
    pub codeblock: CodeblockExpr,
}

impl FunctionDefinitionStmt {
    parser!((expr_parser => ExpressionNode, stmt_parser => StatementNode) => Self {
        let decls_parser = DeclarationStmt::parser()
            .separated_by(just(',').trivia_padded())
            .collect::<Vec<_>>()
            .or_not()
            .map(|opt| opt.unwrap_or_default());

        let param_parser = choice((
            text::keyword("self")
                .trivia_padded()
                .then(just(',').trivia_padded().ignore_then(decls_parser.clone()).or_not())
                .map(|(_, rest)| {
                    (true, rest.unwrap_or_default())
                }),
            decls_parser.map(|decls| (false, decls)),
        ));

        let generics_parser = just('<').trivia_padded()
            .ignore_then(Ident::parser().separated_by(just(',').trivia_padded()).at_least(1).collect::<Vec<_>>())
            .then_ignore(just('>').trivia_padded())
            .or_not()
            .map(|opt| opt.unwrap_or_default());

        Ident::parser()
            .then(generics_parser)
            .then_ignore(just('(').trivia_padded())
            .then(param_parser)
            .then_ignore(just(')').trivia_padded())
            .then_ignore(just("=>").trivia_padded())
            .then(Type::parser().trivia_padded())
            .then(CodeblockExpr::parser(expr_parser, stmt_parser).trivia_padded())
            .map(
                |((((function_name, generics), (is_member_function, params)), return_type), codeblock)| FunctionDefinitionStmt {
                    function_name,
                    generics,
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
