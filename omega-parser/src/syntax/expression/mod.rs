pub mod address_of;
pub mod assignment;
pub mod binary_op;
pub mod codeblock;
pub mod deref;
pub mod field_access;
pub mod function_call;
pub mod index;
pub mod negate;
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
            binary_op::{BinaryOp, BinaryOpExpr},
            codeblock::CodeblockExpr,
            deref::DerefExpr,
            field_access::{FieldAccessExpr, FieldAccessPostfix},
            function_call::{FunctionCallExpr, FunctionCallPostfix},
            index::{IndexExpr, IndexPostfix},
            negate::NegateExpr,
            number::NumberExpr,
            string::StringExpr,
        },
        statement::StatementNode,
    },
};
use crate::syntax::trivia::TriviaExt;
use chumsky::prelude::*;

/// The parser only knows syntax, not semantics: `FieldAccess`/`Index`/`Deref`/
/// `BinaryOp` are just expression-forming operators here, the same as
/// `FunctionCall`. There is no "place"/lvalue concept at this layer --
/// deciding which expression shapes denote an addressable location is HIR
/// lowering's job, and no type-checking happens here either.
#[derive(Debug, Clone)]
pub enum Expression {
    Ident(Ident),
    FieldAccess(Box<FieldAccessExpr>),
    Index(Box<IndexExpr>),
    Deref(Box<DerefExpr>),
    AddressOf(Box<AddressOfExpr>),
    Negate(Box<NegateExpr>),
    BinaryOp(Box<BinaryOpExpr>),
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

/// Binds tighter than the arithmetic operators and assignment, but looser
/// than postfix: `*base`/`&base`/`-base`. So `*p.f` is `*(p.f)` (postfix
/// first), while `(*p).f` needs explicit parens, and `-a * b` is `(-a) * b`
/// -- matching C/Rust precedence.
#[derive(Clone)]
enum Prefix {
    Deref,
    AddressOf,
    Negate,
}

impl Prefix {
    fn into_expression(self, expr: ExpressionNode) -> Expression {
        match self {
            Self::Deref => Expression::Deref(Box::new(DerefExpr { base: expr })),
            Self::AddressOf => Expression::AddressOf(Box::new(AddressOfExpr { base: expr })),
            Self::Negate => Expression::Negate(Box::new(NegateExpr { base: expr })),
        }
    }
}

fn binary_op_expression(left: ExpressionNode, op: BinaryOp, right: ExpressionNode) -> Expression {
    Expression::BinaryOp(Box::new(BinaryOpExpr { left, op, right }))
}

impl ExpressionNode {
    parser!((stmt_parser => StatementNode) => Self {
        recursive(|expr_parser| {
            let primary = choice((
                just('(').trivia_padded()
                    .ignore_then(expr_parser.clone())
                    .then_ignore(just(')').trivia_padded()),
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

            let postfixed = primary.foldl_with(postfix.repeated(), |expr, postfix, extra| {
                ExpressionNode { expression: postfix.into_expression(expr), span: extra.span() }
            });

            let prefix = choice((
                just('*').trivia_padded().to(Prefix::Deref),
                just('&').trivia_padded().to(Prefix::AddressOf),
                just('-').trivia_padded().to(Prefix::Negate),
            ));

            // Right-associative: `prefix.repeated()` collects leading
            // operators left-to-right, and `foldr_with` applies them
            // right-to-left onto `postfixed`, so `**p` is `Deref(Deref(p))`.
            let unary = prefix.repeated().foldr_with(postfixed, |prefix, expr, extra| {
                ExpressionNode { expression: prefix.into_expression(expr), span: extra.span() }
            });

            let mul_op = choice((
                just('*').trivia_padded().to(BinaryOp::Mul),
                just('/').trivia_padded().to(BinaryOp::Div),
                just('%').trivia_padded().to(BinaryOp::Rem),
            ));
            let multiplicative = unary.clone().foldl_with(
                mul_op.then(unary).repeated(),
                |left, (op, right), extra| ExpressionNode {
                    expression: binary_op_expression(left, op, right),
                    span: extra.span(),
                },
            );

            let add_op = choice((
                just('+').trivia_padded().to(BinaryOp::Add),
                just('-').trivia_padded().to(BinaryOp::Sub),
            ));
            let additive = multiplicative.clone().foldl_with(
                add_op.then(multiplicative).repeated(),
                |left, (op, right), extra| ExpressionNode {
                    expression: binary_op_expression(left, op, right),
                    span: extra.span(),
                },
            );

            additive.clone()
                .then(just('=').trivia_padded().ignore_then(expr_parser).or_not())
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
        .trivia_padded()
    });

    pub fn configured_parser<'a>() -> impl Parser<'a, &'a str, Self, ParseError<'a>> + Clone {
        recursive(|expr_parser| Self::parser(StatementNode::parser(expr_parser)))
    }
}
