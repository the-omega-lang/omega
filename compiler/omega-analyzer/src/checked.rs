use crate::resolved_type::{ConstValue, ResolvedFunctionType, ResolvedType};
use omega_hir::{HirId, ModuleId};
use omega_parser::prelude::{BinaryOp, Ident, SelfMode, Span};

#[derive(Debug, Clone)]
pub struct CheckedModule {
    pub id: ModuleId,
    pub items: Vec<CheckedItem>,
}

#[derive(Debug, Clone)]
pub enum CheckedItem {
    Declaration(CheckedDeclaration),
    ExternDeclaration(CheckedExternDeclaration),
    FunctionDefinition(CheckedFunctionDef),
    Struct(CheckedStructDef),
    Enum(CheckedEnumDef),
    Union(CheckedUnionDef),
}

#[derive(Debug, Clone)]
pub struct ExternFunctionRef {
    pub decl_id: HirId,
    pub module_path: Vec<Ident>,
    pub kind: ExternFunctionKind,
    pub fn_type: ResolvedFunctionType,
    pub mangling: crate::annotations::ManglingMode,
}

#[derive(Debug, Clone)]
pub enum ExternFunctionKind {
    Free(Ident),
    Method {
        type_name: Ident,
        method_name: Ident,
    },
    Primitive {
        target: ResolvedType,
        method_name: Ident,
    },
    Conform {
        target: ResolvedType,
        spec_name: Ident,
        spec_args: Vec<ResolvedType>,
        method_name: Ident,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Local,
    Parameter,
    Function,
    Global,
    Comp,
}

#[derive(Debug, Clone)]
pub struct CheckedDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
    pub mutable: bool,
    pub initial_value: Option<ConstValue>,
}

#[derive(Debug, Clone)]
pub struct CheckedExternDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
    pub mangling: crate::annotations::ManglingMode,
}

#[derive(Debug, Clone)]
pub struct CheckedParam {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedField {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedFunctionDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub self_mode: Option<SelfMode>,
    pub is_variadic: bool,
    pub params: Vec<CheckedParam>,
    pub return_type: ResolvedType,
    pub body: CheckedBlock,
    pub inline: Option<crate::annotations::InlineMode>,
    pub mangling: crate::annotations::ManglingMode,
    pub conformance_owner: Option<ConformanceOwner>,
    pub primitive_target: Option<ResolvedType>,
}

#[derive(Debug, Clone)]
pub struct ConformanceOwner {
    pub target: ResolvedType,
    pub spec_module_path: Vec<Ident>,
    pub spec_name: Ident,
    pub spec_args: Vec<ResolvedType>,
    pub monomorphized: bool,
}

impl CheckedFunctionDef {
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self
                .params
                .iter()
                .map(|p| (p.ident.clone(), p.r#type.clone()))
                .collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: self.self_mode,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckedStructDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedField>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct CheckedUnionDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedField>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct CheckedEnumDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub enum CheckedStmt {
    Declaration(CheckedDeclaration),
    ExternDeclaration(CheckedExternDeclaration),
    Expression(CheckedExprNode),
    Return(CheckedExprNode),
    While(CheckedWhile),
    Loop(CheckedLoop),
    For(Box<CheckedFor>),
    Break(CheckedBreak),
    Continue(CheckedContinue),
    Defer(CheckedDefer),
}

#[derive(Debug, Clone)]
pub struct CheckedDefer {
    pub id: HirId,
    pub span: Span,
    pub body: CheckedBlock,
}

#[derive(Debug, Clone)]
pub struct CheckedBreak {
    pub id: HirId,
    pub span: Span,
    pub loop_id: HirId,
}

#[derive(Debug, Clone)]
pub struct CheckedContinue {
    pub id: HirId,
    pub span: Span,
    pub loop_id: HirId,
}

#[derive(Debug, Clone)]
pub struct CheckedBlock {
    pub stmts: Vec<CheckedStmt>,
    pub tail: Option<Box<CheckedExprNode>>,
}

#[derive(Debug, Clone)]
pub struct CheckedWhile {
    pub id: HirId,
    pub span: Span,
    pub condition: CheckedExprNode,
    pub body: CheckedBlock,
}

#[derive(Debug, Clone)]
pub struct CheckedLoop {
    pub id: HirId,
    pub span: Span,
    pub body: CheckedBlock,
    pub has_break: bool,
}

#[derive(Debug, Clone)]
pub struct CheckedFor {
    pub id: HirId,
    pub span: Span,
    pub init: Vec<CheckedStmt>,
    pub condition: CheckedExprNode,
    pub post: Option<CheckedExprNode>,
    pub body: CheckedBlock,
}

#[derive(Debug, Clone)]
pub struct CheckedExprNode {
    pub id: HirId,
    pub span: Span,
    pub r#type: ResolvedType,
    pub kind: CheckedExpr,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumberValue {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
}

#[derive(Debug, Clone)]
pub enum CheckedExpr {
    Place(CheckedPlace),
    Number(NumberValue),
    Bool(bool),
    Char(char),
    String(String),
    ByteString(String),
    FunctionCall(CheckedFunctionCall),
    Assignment(CheckedAssignment),
    CompoundAssign(CheckedCompoundAssign),
    AddressOf(CheckedAddressOf),
    Negate(Box<CheckedExprNode>),
    BitNot(Box<CheckedExprNode>),
    BinaryOp(CheckedBinaryOp),
    Codeblock(CheckedBlock),
    If(CheckedIf),
    ArrayLiteral(CheckedArrayLiteral),
    StructLiteral(CheckedStructLiteral),
    EnumConstruct(CheckedEnumConstruct),
    Slice(CheckedSlice),
    Match(CheckedMatch),
    Cast(CheckedCast),
    Sizeof(ResolvedType),
    UnionConstruct(CheckedUnionConstruct),
    Const(ConstValue),
    SpecCoerce(CheckedSpecCoerce),
    DynamicCall(CheckedDynamicCall),
}

#[derive(Debug, Clone)]
pub struct CheckedSpecCoerce {
    pub base: Box<CheckedExprNode>,
    pub slots: Vec<HirId>,
}

#[derive(Debug, Clone)]
pub struct CheckedDynamicCall {
    pub base: CheckedPlace,
    pub slot_index: usize,
    pub fn_type: ResolvedFunctionType,
    pub args: Vec<CheckedExprNode>,
}

#[derive(Debug, Clone)]
pub struct CheckedUnionConstruct {
    pub field_index: usize,
    pub value: Box<CheckedExprNode>,
}

#[derive(Debug, Clone)]
pub struct CheckedCast {
    pub kind: CastKind,
    pub target_type: ResolvedType,
    pub base: Box<CheckedExprNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    Reinterpret,
    IntExtend { signed: bool },
    IntTruncate,
    IntToFloat { signed: bool },
    FloatToInt { signed: bool },
    FloatExtend,
    FloatTruncate,
    SpecNarrow { slot_offset: usize },
    DropLength,
    Unsize,
}

#[derive(Debug, Clone)]
pub struct CheckedEnumConstruct {
    pub variant_index: usize,
    pub fields: Vec<CheckedStructLiteralField>,
}

#[derive(Debug, Clone)]
pub struct CheckedStructLiteral {
    pub fields: Vec<CheckedStructLiteralField>,
}

#[derive(Debug, Clone)]
pub struct CheckedStructLiteralField {
    pub field_index: usize,
    pub value: CheckedExprNode,
}

#[derive(Debug, Clone)]
pub struct CheckedArrayLiteral {
    pub item_type: ResolvedType,
    pub elements: Vec<CheckedExprNode>,
}

#[derive(Debug, Clone)]
pub struct CheckedSlice {
    pub base: CheckedPlace,
    pub item_type: ResolvedType,
    pub start: Option<Box<CheckedExprNode>>,
    pub end: CheckedRangeEnd,
}

#[derive(Debug, Clone)]
pub enum CheckedRangeEnd {
    Inclusive(Box<CheckedExprNode>),
    Exclusive(Box<CheckedExprNode>),
    Open,
}

#[derive(Debug, Clone)]
pub struct CheckedPlace {
    pub root: CheckedPlaceRoot,
    pub projections: Vec<CheckedProjection>,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub enum CheckedPlaceRoot {
    Variable {
        decl_id: HirId,
        storage: Storage,
        r#type: ResolvedType,
    },
    Expr(Box<CheckedExprNode>),
}

#[derive(Debug, Clone)]
pub enum CheckedProjection {
    FieldAccess {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    Index {
        index_expr: Box<CheckedExprNode>,
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

#[derive(Debug, Clone)]
pub struct CheckedFunctionCall {
    pub callee: Box<CheckedExprNode>,
    pub fn_type: ResolvedFunctionType,
    pub args: Vec<CheckedExprNode>,
}

#[derive(Debug, Clone)]
pub struct CheckedBinaryOp {
    pub op: BinaryOp,
    pub left: Box<CheckedExprNode>,
    pub right: Box<CheckedExprNode>,
}

#[derive(Debug, Clone)]
pub struct CheckedIf {
    pub branches: Vec<(CheckedExprNode, CheckedBlock)>,
    pub else_branch: Option<CheckedBlock>,
}

#[derive(Debug, Clone)]
pub struct CheckedMatch {
    pub arms: Vec<CheckedMatchArm>,
    pub else_branch: Option<CheckedBlock>,
}

#[derive(Debug, Clone)]
pub struct CheckedMatchArm {
    pub conditions: Vec<Vec<CheckedExprNode>>,
    pub body: CheckedBlock,
}

#[derive(Debug, Clone)]
pub struct CheckedAssignment {
    pub target: CheckedPlace,
    pub value: Box<CheckedExprNode>,
}

/// `place op= value` / `place++` / `place--`, represented so `place` is
/// owned exactly once: unlike `CheckedAssignment`, there is no second copy
/// of the place embedded in `value` to read through. MIR lowers `place`
/// once and reuses the resulting address for both the read and the write,
/// per the language's "evaluate the target place only once" rule.
#[derive(Debug, Clone)]
pub struct CheckedCompoundAssign {
    pub place: CheckedPlace,
    /// The coercion `coerce_for_binary_op` would apply to the place's read
    /// value before combining with `value` (e.g. `Pointer` -> `USize`),
    /// mirrored here as data instead of as a cloned expression.
    pub read_cast: Option<(CastKind, ResolvedType)>,
    pub op: BinaryOp,
    pub value: Box<CheckedExprNode>,
    pub result_type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedAddressOf {
    pub place: CheckedPlace,
}
