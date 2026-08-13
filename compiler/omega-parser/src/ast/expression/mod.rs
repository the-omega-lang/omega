pub mod address_of;
pub mod array_literal;
pub mod assignment;
pub mod binary_op;
pub mod bit_not;
pub mod bool_literal;
pub mod byte_string;
pub mod cast;
pub mod char_literal;
pub mod codeblock;
pub mod comp;
pub mod compound_assign;
pub mod deref;
pub mod field_access;
pub mod function_call;
pub mod if_expr;
pub mod incr_decr;
pub mod index;
pub mod macro_invocation;
pub mod match_expr;
pub mod negate;
pub mod number;
pub mod reveal;
pub mod sizeof;
pub mod slice;
pub mod string;
pub mod struct_literal;

use crate::ast::expression::{
    address_of::AddressOfExpr,
    array_literal::ArrayLiteralExpr,
    assignment::AssignmentExpr,
    binary_op::BinaryOpExpr,
    bit_not::BitNotExpr,
    bool_literal::BoolExpr,
    byte_string::ByteStringExpr,
    cast::CastExpr,
    char_literal::CharExpr,
    codeblock::CodeblockExpr,
    comp::CompExpr,
    compound_assign::CompoundAssignExpr,
    deref::DerefExpr,
    field_access::FieldAccessExpr,
    function_call::FunctionCallExpr,
    if_expr::IfExpr,
    incr_decr::{DecrementExpr, IncrementExpr},
    index::IndexExpr,
    macro_invocation::MacroInvocationExpr,
    match_expr::MatchExpr,
    negate::NegateExpr,
    number::NumberExpr,
    reveal::RevealExpr,
    sizeof::SizeofExpr,
    slice::SliceExpr,
    string::StringExpr,
    struct_literal::StructLiteralExpr,
};
use crate::ast::identifier::ExprPath;
use crate::ast::range::RangeExpr;
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
    /// `reveal base` -- see `RevealExpr`'s doc comment.
    Reveal(Box<RevealExpr>),
    /// `comp base` -- see `CompExpr`'s doc comment.
    Comp(Box<CompExpr>),
    Negate(Box<NegateExpr>),
    /// `~base` -- see `BitNotExpr`'s doc comment.
    BitNot(Box<BitNotExpr>),
    /// `<Type>base` -- see `CastExpr`'s doc comment.
    Cast(Box<CastExpr>),
    /// `sizeof<Type>` -- see `SizeofExpr`'s doc comment.
    Sizeof(Box<SizeofExpr>),
    Increment(Box<IncrementExpr>),
    Decrement(Box<DecrementExpr>),
    BinaryOp(Box<BinaryOpExpr>),
    Number(NumberExpr),
    String(StringExpr),
    /// `b"..."` -- a raw run of bytes, not a null-terminated C string: its
    /// type is `*[]u8` (`ResolvedType::Slice`, a data pointer + length),
    /// never `*u8` the way `String` is. See `Context::resolve_pointer_type`'s
    /// `InferredArray` case for why `*[]u8` already means exactly that.
    ByteString(ByteStringExpr),
    Bool(BoolExpr),
    Char(CharExpr),
    Codeblock(CodeblockExpr),
    If(Box<IfExpr>),
    Match(Box<MatchExpr>),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
    /// `target op= value` -- see `CompoundAssignExpr`'s doc comment.
    CompoundAssign(Box<CompoundAssignExpr>),
    ArrayLiteral(ArrayLiteralExpr),
    /// `Name { field = value; ... }` -- see `StructLiteralExpr`'s doc comment.
    StructLiteral(StructLiteralExpr),
    Slice(Box<SliceExpr>),
    /// `name$(arg, ...)` -- expanded away entirely by
    /// `omega_parser::macros::expand` before HIR lowering ever runs; see
    /// `MacroInvocationExpr`'s doc comment.
    MacroInvocation(MacroInvocationExpr),
    /// `a..<b` / `a..=b` / `a..` -- a standalone range, legal *only* as a
    /// range-driven `for` loop's own direct iterator source (`for i in
    /// 10..<20 { ... }`); rejected everywhere else at analysis time. Never
    /// produced by the general expression grammar -- only by the
    /// dedicated `for`-in-iterator parse path (`crate::parser::expression::
    /// parse_range_or_expression`), so this variant is structurally
    /// unreachable anywhere but there. See `crate::ast::range::RangeExpr`'s
    /// doc comment for why this doesn't back a real, general-purpose Range
    /// value type.
    Range(Box<RangeExpr>),
}

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub expression: Expression,
    pub span: Span,
}
