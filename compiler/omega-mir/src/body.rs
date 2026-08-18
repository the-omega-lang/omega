use crate::ids::{BlockId, LocalId};
use omega_analyzer::checked::{CastKind, NumberValue};
use omega_analyzer::resolved_type::{ConstValue, ResolvedFunctionType, ResolvedType};
use omega_hir::{BinaryOp, HirId};
use omega_parser::prelude::{Ident, Span};

#[derive(Debug, Clone)]
pub struct MirBody {
    pub locals: Vec<MirLocalDecl>,
    pub arg_count: usize,
    pub blocks: Vec<MirBlockData>,
}

#[derive(Debug, Clone)]
pub struct MirLocalDecl {
    pub source: Option<HirId>,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct MirBlockData {
    pub statements: Vec<MirExprNode>,
    pub terminator: MirTerminator,
}

#[derive(Debug, Clone)]
pub enum MirTerminator {
    Goto(BlockId),
    Branch {
        condition: MirExprNode,
        then_block: BlockId,
        else_block: BlockId,
    },
    Return(Option<MirExprNode>),
    Unreachable,
}

#[derive(Debug, Clone)]
pub struct MirExprNode {
    pub id: HirId,
    pub span: Span,
    pub r#type: ResolvedType,
    pub kind: MirExpr,
}

#[derive(Debug, Clone)]
pub enum MirExpr {
    Place(MirPlace),
    Number(NumberValue),
    Bool(bool),
    Char(char),
    String(String),
    ByteString(String),
    FunctionCall(MirFunctionCall),
    Assignment(MirAssignment),
    AddressOf(MirAddressOf),
    Negate(Box<MirExprNode>),
    BitNot(Box<MirExprNode>),
    BinaryOp(MirBinaryOp),
    ArrayLiteral(MirArrayLiteral),
    StructLiteral(MirStructLiteral),
    EnumConstruct(MirEnumConstruct),
    Slice(MirSlice),
    Cast(MirCast),
    Sizeof(ResolvedType),
    UnionConstruct(MirUnionConstruct),
    Const(ConstValue),
    SpecCoerce(MirSpecCoerce),
    DynamicCall(MirDynamicCall),
}

#[derive(Debug, Clone)]
pub struct MirFunctionCall {
    pub callee: Box<MirExprNode>,
    pub fn_type: ResolvedFunctionType,
    pub args: Vec<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirAssignment {
    pub target: MirPlace,
    pub value: Box<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirAddressOf {
    pub place: MirPlace,
}

#[derive(Debug, Clone)]
pub struct MirBinaryOp {
    pub op: BinaryOp,
    pub left: Box<MirExprNode>,
    pub right: Box<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirArrayLiteral {
    pub item_type: ResolvedType,
    pub elements: Vec<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirStructLiteral {
    pub fields: Vec<MirFieldInit>,
}

#[derive(Debug, Clone)]
pub struct MirFieldInit {
    pub field_index: usize,
    pub value: MirExprNode,
}

#[derive(Debug, Clone)]
pub struct MirEnumConstruct {
    pub variant_index: usize,
    pub fields: Vec<MirFieldInit>,
}

#[derive(Debug, Clone)]
pub struct MirUnionConstruct {
    pub field_index: usize,
    pub value: Box<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirSlice {
    pub base: MirPlace,
    pub item_type: ResolvedType,
    pub start: Option<Box<MirExprNode>>,
    pub end: Option<Box<MirExprNode>>,
    pub inclusive: bool,
}

#[derive(Debug, Clone)]
pub struct MirCast {
    pub kind: CastKind,
    pub target_type: ResolvedType,
    pub base: Box<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirSpecCoerce {
    pub base: Box<MirExprNode>,
    pub slots: Vec<HirId>,
}

#[derive(Debug, Clone)]
pub struct MirDynamicCall {
    pub base: MirPlace,
    pub slot_index: usize,
    pub fn_type: ResolvedFunctionType,
    pub args: Vec<MirExprNode>,
}

#[derive(Debug, Clone)]
pub struct MirPlace {
    pub root: MirPlaceRoot,
    pub projections: Vec<MirProjection>,
    pub r#type: ResolvedType,
    pub align: u32,
}

#[derive(Debug, Clone)]
pub enum MirPlaceRoot {
    Local { id: LocalId, r#type: ResolvedType },
    Function(HirId),
    Global { id: HirId, r#type: ResolvedType },
    Expr(Box<MirExprNode>),
}

#[derive(Debug, Clone)]
pub enum MirProjection {
    FieldAccess {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    Index {
        index_expr: Box<MirExprNode>,
        item_type: ResolvedType,
    },
    Deref {
        r#type: ResolvedType,
    },
    SliceLength,
    SpecObjectPtr {
        mutable: bool,
    },
    SpecObjectVtable,
    EnumTag {
        r#type: ResolvedType,
    },
    EnumHeader {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    EnumDynamicField {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    EnumBody {
        variant_index: usize,
        field_index: usize,
        r#type: ResolvedType,
    },
    UnionField {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
}
