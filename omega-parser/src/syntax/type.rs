use crate::syntax::{ParseError, SyntaxParser, identifier::Ident};
use chumsky::prelude::*;

#[derive(Debug, Clone)]
pub enum Type {
    Named(Ident), // Identifier types. Example: void, i32, i64, char, ...
    Pointer(Box<Type>),
    Function {
        params: Vec<(Ident, Type)>,
        return_type: Box<Type>,
    },
}

impl SyntaxParser for Type {
    fn parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
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
                .then_ignore(just(')').padded())
                .then_ignore(just("=>").padded())
                .then(parser)
                .map(|(params, rettype)| Type::Function {
                    params: params,
                    return_type: Box::new(rettype),
                });

            choice((named_type_parser, pointer_parser, function_parser)).padded()
        })
    }
}
