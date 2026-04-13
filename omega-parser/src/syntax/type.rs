use crate::{
    parser,
    syntax::{ParseError, identifier::Ident},
};
use chumsky::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<(Ident, Type)>,
    pub return_type: Box<Type>,
    pub is_variadic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Named(Ident), // Identifier types. Example: void, i32, i64, char, ...
    Pointer(Box<Type>),
    Function(FunctionType),
}

impl Type {
    parser!(() => Self {
        recursive(|parser| {
            let ident_parser = Ident::parser();
            let named_type_parser = ident_parser.clone().map(|ident| Type::Named(ident));

            let pointer_parser = just('*')
                .ignore_then(parser.clone())
                .map(|ptr: Type| Type::Pointer(Box::new(ptr)));

            let param_parser = ident_parser
                .clone()
                .then_ignore(just(':').padded())
                .then(parser.clone())
                .map(|(ident, typ)| (ident, typ));

            let function_parser = just('(')
                .padded()
                .ignore_then(param_parser.separated_by(just(',').padded()).collect())
                .then(just(',').padded().ignore_then(just("...").padded().ignored()).or_not())
                .then_ignore(just(')').padded())
                .then_ignore(just("=>").padded())
                .then(parser)
                .map(|((params, is_variadic), rettype)| {
                    Type::Function(FunctionType {
                        params: params,
                        return_type: Box::new(rettype),
                        is_variadic: is_variadic.is_some()
                    })
                });

            choice((named_type_parser, pointer_parser, function_parser)).padded()
        })
    });
}
