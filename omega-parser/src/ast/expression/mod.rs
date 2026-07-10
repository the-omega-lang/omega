pub mod address_of;
pub mod array_literal;
pub mod assignment;
pub mod binary_op;
pub mod bool_literal;
pub mod char_literal;
pub mod codeblock;
pub mod deref;
pub mod field_access;
pub mod function_call;
pub mod if_expr;
pub mod incr_decr;
pub mod index;
pub mod macro_invocation;
pub mod negate;
pub mod number;
pub mod slice;
pub mod string;
pub mod struct_literal;

use crate::ast::identifier::ExprPath;
use crate::ast::expression::{
    address_of::AddressOfExpr, array_literal::ArrayLiteralExpr, assignment::AssignmentExpr,
    binary_op::BinaryOpExpr, bool_literal::BoolExpr, char_literal::CharExpr,
    codeblock::CodeblockExpr, deref::DerefExpr, field_access::FieldAccessExpr,
    function_call::FunctionCallExpr, if_expr::IfExpr, incr_decr::{DecrementExpr, IncrementExpr},
    index::IndexExpr, macro_invocation::MacroInvocationExpr, negate::NegateExpr,
    number::NumberExpr, slice::SliceExpr, string::StringExpr, struct_literal::StructLiteralExpr,
};
use crate::diagnostics::Span;

/// The parser only knows syntax, not semantics: `FieldAccess`/`Index`/`Deref`/
/// `BinaryOp` are just expression-forming operators here, the same as
/// `FunctionCall`. There is no "place"/lvalue concept at this layer --
/// deciding which expression shapes denote an addressable location is HIR
/// lowering's job, and no type-checking happens here either.
#[derive(Debug, Clone)]
pub enum Expression {
    /// A (possibly module-qualified) path -- `foo`, or `mymodule::thing::foo`,
    /// or one with explicit generic arguments on a segment
    /// (`Optional<u32>::Some`). A bare, unqualified name is just the
    /// degenerate one-segment case; see `Path`/`ExprPath`'s own doc comments.
    Path(ExprPath),
    FieldAccess(Box<FieldAccessExpr>),
    Index(Box<IndexExpr>),
    Deref(Box<DerefExpr>),
    AddressOf(Box<AddressOfExpr>),
    Negate(Box<NegateExpr>),
    Increment(Box<IncrementExpr>),
    Decrement(Box<DecrementExpr>),
    BinaryOp(Box<BinaryOpExpr>),
    Number(NumberExpr),
    String(StringExpr),
    Bool(BoolExpr),
    Char(CharExpr),
    Codeblock(CodeblockExpr),
    If(Box<IfExpr>),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
    ArrayLiteral(ArrayLiteralExpr),
    /// `Name { field: value; ... }` -- see `StructLiteralExpr`'s doc comment.
    StructLiteral(StructLiteralExpr),
    Slice(Box<SliceExpr>),
    /// `name!(arg, ...)` -- expanded away entirely by
    /// `omega_parser::macros::expand` before HIR lowering ever runs; see
    /// `MacroInvocationExpr`'s doc comment.
    MacroInvocation(MacroInvocationExpr),
}

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub expression: Expression,
    pub span: Span,
}
