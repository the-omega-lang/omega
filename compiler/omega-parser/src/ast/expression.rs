use crate::ast::identifier::{ExprPath, Ident, Origin};
use crate::ast::range::RangeExpr;
use crate::ast::statement::StatementNode;
use crate::ast::r#type::Type;
use crate::diagnostics::Span;
use crate::lexer::Token;

#[derive(Debug, Clone)]
pub enum Expression {
    Path(ExprPath),
    FieldAccess(Box<FieldAccessExpr>),
    Index(Box<IndexExpr>),
    Deref(Box<DerefExpr>),
    AddressOf(Box<AddressOfExpr>),
    Reveal(Box<RevealExpr>),
    Comp(Box<CompExpr>),
    Negate(Box<NegateExpr>),
    BitNot(Box<BitNotExpr>),
    Not(Box<NotExpr>),
    Logical(Box<LogicalExpr>),
    Cast(Box<CastExpr>),
    Sizeof(Box<SizeofExpr>),
    Increment(Box<IncrementExpr>),
    Decrement(Box<DecrementExpr>),
    BinaryOp(Box<BinaryOpExpr>),
    Number(NumberExpr),
    String(StringExpr),
    ByteString(ByteStringExpr),
    Bool(BoolExpr),
    Char(CharExpr),
    Codeblock(CodeblockExpr),
    If(Box<IfExpr>),
    Match(Box<MatchExpr>),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
    CompoundAssign(Box<CompoundAssignExpr>),
    ArrayLiteral(ArrayLiteralExpr),
    StructLiteral(StructLiteralExpr),
    Slice(Box<SliceExpr>),
    MacroInvocation(MacroInvocationExpr),
    Range(Box<RangeExpr>),
    Try(Box<TryExpr>),
}

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub expression: Expression,
    pub span: Span,
    /// The syntax owner of this construct: the provenance of the token that
    /// introduces it. Default for ordinary source, a macro expansion's origin
    /// for template-authored syntax, and the caller's origin for syntax
    /// substituted into an expansion.
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct FieldAccessExpr {
    pub base: ExpressionNode,
    pub field: Ident,
    /// The `.field` token's own provenance, which is the macro definition
    /// site for a macro-authored member name and the caller's for a
    /// substituted one. Member visibility is decided with these rights.
    pub field_origin: Origin,
}

#[derive(Debug, Clone)]
pub struct IndexExpr {
    pub base: ExpressionNode,
    pub index: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct DerefExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct AddressOfExpr {
    pub base: ExpressionNode,
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct RevealExpr {
    pub base: ExpressionNode,
    /// The `reveal` keyword's own expansion origin. A `reveal` authorizes
    /// only references sharing it, so the bypass never crosses the boundary
    /// between a macro body and its caller in either direction.
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct CompExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct NegateExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct BitNotExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct NotExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct LogicalExpr {
    pub op: LogicalOp,
    pub left: ExpressionNode,
    pub right: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct CastExpr {
    pub target: Type,
    pub base: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct SizeofExpr {
    pub r#type: Type,
}

#[derive(Debug, Clone)]
pub struct IncrementExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct DecrementExpr {
    pub base: ExpressionNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::Shl => "<<",
            Self::Shr => ">>",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BinaryOpExpr {
    pub left: ExpressionNode,
    pub op: BinaryOp,
    pub right: ExpressionNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberBase {
    Decimal,
    Hex,
    Octal,
    Binary,
}

impl NumberBase {
    pub fn radix(self) -> u32 {
        match self {
            Self::Decimal => 10,
            Self::Hex => 16,
            Self::Octal => 8,
            Self::Binary => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NumberExpr {
    pub base: NumberBase,
    pub integer_part: String,
    pub fractional_part: Option<String>,
    pub explicit_type: Option<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StringExpr(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ByteStringExpr(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoolExpr(pub bool);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharExpr(pub char);

#[derive(Debug, Clone)]
pub struct CodeblockExpr {
    pub statements: Vec<StatementNode>,
    pub tail: Option<Box<ExpressionNode>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct IfExpr {
    pub branches: Vec<(ExpressionNode, CodeblockExpr)>,
    pub else_branch: Option<CodeblockExpr>,
}

#[derive(Debug, Clone)]
pub struct MatchExpr {
    pub scrutinee: ExpressionNode,
    pub arms: Vec<MatchArm>,
    pub else_branch: Option<CodeblockExpr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: ExpressionNode,
    pub span: Span,
}

/// A match arm's pattern carries both readings the syntax allows, because
/// which one is meant depends on the scrutinee's semantic type. `value` is
/// the ordinary value/range reading and `r#type` a complete type parse that
/// reached the arm's `=>`; the analyzer selects `r#type` only for an
/// anonymous-enum scrutinee. At least one of the two is always present.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub value: Option<PatternValue>,
    pub r#type: Option<Type>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum PatternValue {
    Value(ExpressionNode),
    Range(RangeExpr),
}

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub callee: Box<ExpressionNode>,
    pub args: Vec<ExpressionNode>,
}

#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub target: ExpressionNode,
    pub value: Box<ExpressionNode>,
}

#[derive(Debug, Clone)]
pub struct CompoundAssignExpr {
    pub target: ExpressionNode,
    pub op: BinaryOp,
    pub value: Box<ExpressionNode>,
}

#[derive(Debug, Clone)]
pub struct ArrayLiteralExpr {
    pub elements: Vec<ExpressionNode>,
}

#[derive(Debug, Clone)]
pub struct StructLiteralExpr {
    pub path: ExprPath,
    pub fields: Vec<StructLiteralField>,
}

#[derive(Debug, Clone)]
pub struct StructLiteralField {
    pub name: Ident,
    pub name_span: Span,
    pub name_origin: Origin,
    pub value: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct SliceExpr {
    pub base: ExpressionNode,
    pub range: RangeExpr,
}

/// A postfix `base?`. `operator_span` covers only the `?` token so
/// diagnostics and tooling can point at the operator rather than the whole
/// expression.
#[derive(Debug, Clone)]
pub struct TryExpr {
    pub base: ExpressionNode,
    pub operator_span: Span,
}

#[derive(Debug, Clone)]
pub struct MacroInvocationExpr {
    pub name: Ident,
    pub args: Vec<Vec<Token>>,
    pub origin: Origin,
}
