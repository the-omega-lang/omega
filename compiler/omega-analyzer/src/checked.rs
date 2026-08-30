use crate::resolved_type::{
    CallingConvention, ConstValue, ResolvedFunctionParam, ResolvedFunctionType, ResolvedGenericArg,
    ResolvedType,
};
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
    ForeignBinding(CheckedForeignBinding),
    ForeignFunction(CheckedForeignFunctionDef),
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
        spec_args: Vec<ResolvedGenericArg>,
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
pub struct CheckedForeignBinding {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
    pub mangling: crate::annotations::ManglingMode,
}

/// A direct `foreign(cc) name(...) => T;`/`{ ... }` item. Kept separate from
/// `CheckedFunctionDef` because a declaration has no body, unlike every
/// ordinary Omega function; `body: None` is a declaration, `Some` a
/// definition (e.g. a mangled `foreign(c)` callback).
#[derive(Debug, Clone)]
pub struct CheckedForeignFunctionDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub calling_convention: CallingConvention,
    pub is_variadic: bool,
    pub params: Vec<CheckedParam>,
    pub return_type: ResolvedType,
    pub body: Option<CheckedBlock>,
    pub mangling: crate::annotations::ManglingMode,
}

impl CheckedForeignFunctionDef {
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self
                .params
                .iter()
                .map(|p| ResolvedFunctionParam::described(p.ident.clone(), p.r#type.clone()))
                .collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: None,
            calling_convention: self.calling_convention,
        }
    }
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
    pub generic_args: Vec<ResolvedGenericArg>,
    pub self_mode: Option<SelfMode>,
    pub is_variadic: bool,
    pub params: Vec<CheckedParam>,
    pub return_type: ResolvedType,
    pub body: CheckedBlock,
    pub inline: Option<crate::annotations::InlineMode>,
    pub mangling: crate::annotations::ManglingMode,
    pub conformance_owner: Option<ConformanceOwner>,
    pub primitive_target: Option<ResolvedType>,
    pub naked: bool,
}

#[derive(Debug, Clone)]
pub struct ConformanceOwner {
    pub target: ResolvedType,
    pub spec_module_path: Vec<Ident>,
    pub spec_name: Ident,
    pub spec_args: Vec<ResolvedGenericArg>,
    pub monomorphized: bool,
}

impl CheckedFunctionDef {
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self
                .params
                .iter()
                .map(|p| ResolvedFunctionParam::described(p.ident.clone(), p.r#type.clone()))
                .collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: self.self_mode,
            calling_convention: CallingConvention::Omega,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckedStructDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub generic_args: Vec<ResolvedGenericArg>,
    pub fields: Vec<CheckedField>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct CheckedUnionDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub generic_args: Vec<ResolvedGenericArg>,
    pub fields: Vec<CheckedField>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct CheckedEnumDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub generic_args: Vec<ResolvedGenericArg>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub enum CheckedStmt {
    Declaration(CheckedDeclaration),
    Expression(CheckedExprNode),
    Return(CheckedExprNode),
    While(CheckedWhile),
    Loop(CheckedLoop),
    For(Box<CheckedFor>),
    Break(CheckedBreak),
    Continue(CheckedContinue),
    Defer(CheckedDefer),
    InlineAsm(CheckedInlineAsm),
}

#[derive(Debug, Clone)]
pub struct CheckedInlineAsm {
    pub id: HirId,
    pub span: Span,
    pub descriptors: Vec<CheckedAsmDescriptor>,
    pub body: String,
    pub body_span: Span,
}

#[derive(Debug, Clone)]
pub struct CheckedAsmDescriptor {
    pub span: Span,
    /// The source `$name` this descriptor is reachable by, when one can be
    /// inferred (`reg(x)`/`reg(&x)`/`reg(&mut x)` -> `x`; `comp(NAME)` ->
    /// `NAME`). `clobber` descriptors are never bindable and always `None`.
    pub binding_name: Option<Ident>,
    pub kind: CheckedAsmDescriptorKind,
}

#[derive(Debug, Clone)]
pub enum CheckedAsmDescriptorKind {
    Reg {
        expr: CheckedExprNode,
        physical: Option<String>,
    },
    Comp {
        text: String,
    },
    Clobber {
        register: String,
    },
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
    AnonymousEnumWiden(CheckedAnonymousEnumWiden),
    DynamicCall(CheckedDynamicCall),
    Try(CheckedTry),
}

/// A source-level `expression?`. Analysis resolves every fact the operator
/// needs -- which canonical fallible type the operand is, which variants and
/// payload fields carry success and failure, and how the operand's failure
/// reaches the enclosing function's failure type -- and records them here.
/// The operator survives as itself so checked-tree consumers still see what
/// the user wrote; turning it into branches and a return is MIR's job.
///
/// The node's own type is the success payload type, which is independent of
/// the enclosing function's success type.
#[derive(Debug, Clone)]
pub struct CheckedTry {
    pub operand: Box<CheckedExprNode>,
    pub operator_span: Span,
    pub kind: CheckedTryKind,
    pub source: CheckedTrySource,
    pub destination: CheckedTryDestination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedTryKind {
    Option,
    Result,
}

impl CheckedTryKind {
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Option => "Option",
            Self::Result => "Result",
        }
    }
}

/// The operand's resolved shape. `tag_type`/`success_tag` say how to tell the
/// two variants apart, so no consumer re-derives them from the declaration.
#[derive(Debug, Clone)]
pub struct CheckedTrySource {
    pub tag_type: ResolvedType,
    pub success_variant: usize,
    pub success_tag: NumberValue,
    pub success_field: usize,
    pub failure_variant: usize,
    /// `Err.error`'s field index and type. `None` for `Option::None`, which
    /// carries no payload.
    pub failure_payload: Option<(usize, ResolvedType)>,
}

/// The enclosing function's fallible return type and the failure value the
/// operator builds in it.
///
/// The canonical fallible enums declare no dynamic fields, so `failure_field`
/// reads the same whether a consumer treats it as an index into the variant's
/// own fields (a body projection) or into an enum construction, which counts
/// any dynamic fields first.
#[derive(Debug, Clone)]
pub struct CheckedTryDestination {
    pub r#type: ResolvedType,
    pub failure_variant: usize,
    pub failure_field: Option<usize>,
    /// How the operand's `E` becomes the destination's `F`; identity for
    /// `Option` and for an already-matching `Result` error type.
    pub error_coercion: CheckedCoercion,
}

/// A conversion from a found type to an expected type that the analyzer has
/// already decided is legal, recorded as data rather than applied to an
/// expression. Sites that own a value hand it to `apply_coercion`; sites like
/// `CheckedTry` that only own a value's *description* store the plan and let
/// MIR or compile-time evaluation replay it. Either way the legality
/// question is answered exactly once, here.
#[derive(Debug, Clone, Default)]
pub struct CheckedCoercion {
    pub steps: Vec<CheckedCoercionStep>,
}

impl CheckedCoercion {
    pub fn is_identity(&self) -> bool {
        self.steps.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum CheckedCoercionStep {
    /// Reads the proven member out of a refined anonymous-enum value. The
    /// storage is unchanged; this only names the member inside it.
    ProjectAnonymousMember {
        variant_index: usize,
        member_type: ResolvedType,
    },
    /// Packs an exact member value into an anonymous enum, tagged with its
    /// canonical index.
    InjectAnonymousMember {
        variant_index: usize,
        target_type: ResolvedType,
    },
    /// Rebuilds an anonymous-enum value under a wider shape's tags.
    WidenAnonymousEnum {
        variant_map: Vec<usize>,
        target_type: ResolvedType,
    },
    /// Pairs a pointer with the vtable slots proving its pointee implements
    /// the destination shape.
    SpecCoerce {
        slots: Vec<HirId>,
        target_type: ResolvedType,
    },
}

/// Rebuilds an anonymous-enum value under a wider shape's tags. Both shapes
/// are canonically ordered independently, so a destination index is not
/// derivable from a source index: `variant_map` carries the analyzer's
/// decision, holding the destination canonical index for each source
/// canonical index.
#[derive(Debug, Clone)]
pub struct CheckedAnonymousEnumWiden {
    pub source: Box<CheckedExprNode>,
    pub variant_map: Vec<usize>,
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
    Discard,
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
