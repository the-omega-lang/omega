use crate::{
    parser,
    syntax::identifier::Ident,
};
use crate::syntax::trivia::TriviaExt;
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
    /// `[T]` -- an unsized run of `T`, only ever meaningful today as a
    /// parameter type used the way C's decayed array parameters are (see
    /// `argv : [*u8]` in `examples/dev/main.omg`): a single thin pointer
    /// value, with no length carried alongside it. `*[T]` is the pointer
    /// form of this and is *not* `Pointer(Array(T))` -- see
    /// `Context::resolve_type`'s special case, which turns that combination
    /// into `ResolvedType::Slice` (a fat pointer) instead, per the
    /// language's actual slice design.
    Array(Box<Type>),
    /// `[T; N]` -- a sized, inline, contiguous run of exactly `N` `T`s. `N`
    /// is kept as raw digit text here and parsed/range-checked during type
    /// resolution (`Context::resolve_type`), the same way `NumberExpr`'s
    /// integer literals are kept as text until semantic analysis -- the
    /// parser never rejects input on its own.
    SizedArray(Box<Type>, String),
}

impl Type {
    parser!(() => Self {
        recursive(|parser| {
            let ident_parser = Ident::parser();
            let named_type_parser = ident_parser.clone().map(|ident| Type::Named(ident));

            let pointer_parser = just('*')
                .ignore_then(parser.clone())
                .map(|ptr: Type| Type::Pointer(Box::new(ptr)));

            let array_size = text::digits(10).at_least(1).to_slice().map(ToString::to_string);
            let array_parser = just('[')
                .trivia_padded()
                .ignore_then(parser.clone())
                .then(just(';').trivia_padded().ignore_then(array_size).or_not())
                .then_ignore(just(']'))
                .map(|(subtype, size)| match size {
                    Some(size) => Type::SizedArray(Box::new(subtype), size),
                    None => Type::Array(Box::new(subtype)),
                });

            // TODO: Reuse the declaration parser here
            let decl_parser = ident_parser
                .clone()
                .then_ignore(just(':').trivia_padded())
                .then(parser.clone())
                .map(|(ident, typ)| (ident, typ));

            let decls_parser = decl_parser
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
            )).trivia_padded();

            let function_parser = just('(')
                .trivia_padded()
                .ignore_then(param_parser)
                .then(just(',').trivia_padded().ignore_then(just("...").trivia_padded().ignored()).or_not())
                .then_ignore(just(')').trivia_padded())
                .then_ignore(just("=>").trivia_padded())
                .then(parser)
                .map(|(((is_member_function, params), is_variadic), rettype)| {
                    Type::Function(FunctionType {
                        params,
                        return_type: Box::new(rettype),
                        is_variadic: is_variadic.is_some(),
                        is_member_function
                    })
                });

            choice((pointer_parser, array_parser, function_parser, named_type_parser)).trivia_padded()
        })
    });
}
