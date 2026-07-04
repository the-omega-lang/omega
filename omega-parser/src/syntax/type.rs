use crate::{
    parser,
    syntax::identifier::Ident,
};
use chumsky::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<(Ident, Type)>,
    pub return_type: Box<Type>,
    pub is_variadic: bool,
    pub is_member_function: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Named(Ident), // Identifier types. Example: void, i32, i64, char, ...
    Pointer(Box<Type>),
    Function(FunctionType),
    Array(Box<Type>),
}

impl Type {
    parser!(() => Self {
        recursive(|parser| {
            let ident_parser = Ident::parser();
            let named_type_parser = ident_parser.clone().map(|ident| Type::Named(ident));

            let pointer_parser = just('*')
                .ignore_then(parser.clone())
                .map(|ptr: Type| Type::Pointer(Box::new(ptr)));

            let array_parser = just('[')
                .ignore_then(parser.clone())
                .then_ignore(just(']'))
                .map(|subtype| Type::Array(Box::new(subtype)));

            // TODO: Reuse the declaration parser here
            let decl_parser = ident_parser
                .clone()
                .then_ignore(just(':').padded())
                .then(parser.clone())
                .map(|(ident, typ)| (ident, typ));

            let decls_parser = decl_parser
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
            )).padded();

            let function_parser = just('(')
                .padded()
                .ignore_then(param_parser)
                .then(just(',').padded().ignore_then(just("...").padded().ignored()).or_not())
                .then_ignore(just(')').padded())
                .then_ignore(just("=>").padded())
                .then(parser)
                .map(|(((is_member_function, params), is_variadic), rettype)| {
                    Type::Function(FunctionType {
                        params,
                        return_type: Box::new(rettype),
                        is_variadic: is_variadic.is_some(),
                        is_member_function
                    })
                });

            choice((pointer_parser, array_parser, function_parser, named_type_parser)).padded()
        })
    });
}
