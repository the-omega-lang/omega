pub mod address_of;
pub mod assignment;
pub mod codeblock;
pub mod deref;
pub mod field_access;
pub mod function_call;
pub mod index;
pub mod number;
pub mod string;

use crate::{
    parser,
    prelude::Ident,
    syntax::{
        ParseError,
        expression::{
            address_of::AddressOfExpr,
            assignment::AssignmentExpr,
            codeblock::CodeblockExpr,
            deref::DerefExpr,
            field_access::{FieldAccessExpr, FieldAccessPostfix},
            function_call::{FunctionCallExpr, FunctionCallPostfix},
            index::{IndexExpr, IndexPostfix},
            number::NumberExpr,
            string::StringExpr,
        },
        statement::StatementNode,
    },
};
use chumsky::prelude::*;

/// The parser only knows syntax, not semantics: `FieldAccess`/`Index`/`Deref`
/// are just expression-forming operators here, the same as `FunctionCall`.
/// There is no "place"/lvalue concept at this layer -- deciding which
/// expression shapes denote an addressable location is HIR lowering's job.
#[derive(Debug, Clone)]
pub enum Expression {
    Ident(Ident),
    FieldAccess(Box<FieldAccessExpr>),
    Index(Box<IndexExpr>),
    Deref(Box<DerefExpr>),
    AddressOf(Box<AddressOfExpr>),
    Number(NumberExpr),
    String(StringExpr),
    Codeblock(CodeblockExpr),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
}

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub expression: Expression,
    pub span: SimpleSpan,
}

/// Binds tightest: `.field`, `[index]`, `(args)`.
enum Postfix {
    Call(FunctionCallPostfix),
    FieldAccess(FieldAccessPostfix),
    Index(IndexPostfix),
}

impl Postfix {
    fn into_expression(self, expr: ExpressionNode) -> Expression {
        match self {
            Self::Call(x) => Expression::FunctionCall(FunctionCallExpr {
                callee: Box::new(expr),
                args: x.args,
            }),
            Self::FieldAccess(x) => Expression::FieldAccess(Box::new(FieldAccessExpr {
                base: expr,
                field: x.field,
            })),
            Self::Index(x) => Expression::Index(Box::new(IndexExpr {
                base: expr,
                index: x.index,
            })),
        }
    }
}

/// Binds tighter than assignment but looser than postfix: `*base`/`&base`.
/// So `*p.f` is `*(p.f)` (postfix first), while `(*p).f` needs explicit
/// parens -- matching C/Rust precedence.
#[derive(Clone)]
enum Prefix {
    Deref,
    AddressOf,
}

impl Prefix {
    fn into_expression(self, expr: ExpressionNode) -> Expression {
        match self {
            Self::Deref => Expression::Deref(Box::new(DerefExpr { base: expr })),
            Self::AddressOf => Expression::AddressOf(Box::new(AddressOfExpr { base: expr })),
        }
    }
}

impl ExpressionNode {
    parser!((stmt_parser => StatementNode) => Self {
        recursive(|expr_parser| {
            let primary = choice((
                just('(').padded()
                    .ignore_then(expr_parser.clone())
                    .then_ignore(just(')').padded()),
                CodeblockExpr::parser(stmt_parser).map(Expression::Codeblock)
                    .map_with(|expression, extra| ExpressionNode { expression, span: extra.span() }),
                NumberExpr::parser().map(Expression::Number)
                    .map_with(|expression, extra| ExpressionNode { expression, span: extra.span() }),
                StringExpr::parser().map(Expression::String)
                    .map_with(|expression, extra| ExpressionNode { expression, span: extra.span() }),
                Ident::parser().map(Expression::Ident)
                    .map_with(|expression, extra| ExpressionNode { expression, span: extra.span() }),
            ));

            let postfix = choice((
                FunctionCallPostfix::parser(expr_parser.clone()).map(Postfix::Call),
                FieldAccessPostfix::parser().map(Postfix::FieldAccess),
                IndexPostfix::parser(expr_parser.clone()).map(Postfix::Index),
            ));

            let postfixed = primary
                .then(postfix.repeated().collect())
                .map_with(|(expr, postfixes): (ExpressionNode, Vec<Postfix>), extra| {
                    let mut expression = expr;
                    for postfix in postfixes {
                        let expr = postfix.into_expression(expression);
                        expression = ExpressionNode {
                            expression: expr, span: extra.span()
                        }
                    }

                    expression
                });

            let prefix = choice((
                just('*').padded().to(Prefix::Deref),
                just('&').padded().to(Prefix::AddressOf),
            ));

            let unary = prefix.repeated().collect::<Vec<_>>()
                .then(postfixed)
                .map_with(|(prefixes, expr): (Vec<Prefix>, ExpressionNode), extra| {
                    let mut expression = expr;
                    // Prefixes bind right-to-left: `**p` collects `[Deref, Deref]`
                    // (leftmost first), and the *rightmost* one (closest to `p`)
                    // is applied first, so iterate in reverse.
                    for prefix in prefixes.into_iter().rev() {
                        let expr = prefix.into_expression(expression);
                        expression = ExpressionNode {
                            expression: expr, span: extra.span()
                        }
                    }

                    expression
                });

            unary.clone()
                .then(just('=').padded().ignore_then(expr_parser).or_not())
                .map_with(|(target, value), extra| match value {
                    Some(value) => ExpressionNode {
                        expression: Expression::Assignment(Box::new(AssignmentExpr {
                            target,
                            value: Box::new(value),
                        })),
                        span: extra.span(),
                    },
                    None => target,
                })
        })
        .padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|expr_parser| Self::parser(StatementNode::parser(expr_parser)))
    }
}
