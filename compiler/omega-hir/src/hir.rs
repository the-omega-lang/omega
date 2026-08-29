use crate::ids::HirId;
pub use omega_parser::prelude::{AliasTarget, BinaryOp, LogicalOp};
use omega_parser::prelude::{
    ByteStringExpr, ExprPath, FunctionType, FunctionTypeParam, Ident, NumberExpr, Origin, Path,
    RawConvention, SelfMode, Span, StringExpr, Type, Visibility,
};

#[derive(Debug, Clone)]
pub struct HirAnnotation {
    pub name: Ident,
    pub args: Vec<HirAnnotationArg>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirAnnotationArg {
    Ident(Ident),
    KeyValue(Ident, HirAnnotationValue),
}

#[derive(Debug, Clone)]
pub enum HirAnnotationValue {
    IntLiteral(String),
    Sizeof(Type),
    StrLiteral(String),
}

#[derive(Debug, Clone)]
pub struct HirModule {
    pub id: crate::ids::ModuleId,
    pub items: Vec<HirItem>,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Declaration {
        decl: HirDeclaration,
        visibility: Visibility,
    },
    DeclarationWithInit {
        decl: HirDeclaration,
        value: HirExprNode,
        visibility: Visibility,
    },
    Walrus {
        walrus: HirWalrusDeclaration,
        visibility: Visibility,
    },
    ForeignBinding(HirForeignBinding),
    ForeignFunction(HirForeignFunction),
    FunctionDefinition(HirFunctionDef),
    Struct(HirStructDef),
    Enum(HirEnumDef),
    Union(HirUnionDef),
    Spec(HirSpecDef),
    Gap(HirGapDef),
    Glue(HirGlueDef),
    Conform(HirConformDef),
    Primitive(HirPrimitiveDef),
    Import(HirImport),
    Alias(HirAlias),
}

/// A compile-time-only second name for an existing declaration. The target is
/// kept structurally unresolved: only semantic analysis can tell whether a
/// path names a module, type, function, or macro, and only the use site can
/// tell a static spec bound from a dynamic spec object.
#[derive(Debug, Clone)]
pub struct HirAlias {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub target_span: Span,
    pub name: Ident,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub generics: Vec<HirGenericParam>,
    pub target: AliasTarget,
}

#[derive(Debug, Clone)]
pub struct HirGenericParam {
    pub ident: Ident,
    pub bounds: Vec<Type>,
    pub default: Option<Type>,
}

#[derive(Debug, Clone)]
/// One terminal binding of an import tree. Brace groups do not survive
/// lowering: each leaf becomes an independent import of `path` bound as
/// `name`, carrying the `reveal` it inherited from its ancestors.
pub struct HirImport {
    pub id: HirId,
    pub span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub reveal: bool,
    pub name: Ident,
    pub path: Path,
}

#[derive(Debug, Clone)]
pub struct HirDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub origin: Origin,
    pub r#type: Type,
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct HirForeignBinding {
    pub id: HirId,
    pub span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub ident: Ident,
    pub name_span: Span,
    pub r#type: Type,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
}

#[derive(Debug, Clone)]
pub struct HirForeignFunction {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub convention: Option<RawConvention>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub params: Vec<HirParam>,
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<HirBlock>,
}

impl HirForeignFunction {
    pub fn function_type(&self) -> FunctionType {
        let params = self
            .params
            .iter()
            .map(|p| FunctionTypeParam::described(p.ident.clone(), p.span, p.r#type.clone()))
            .collect::<Vec<_>>();

        FunctionType {
            params,
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: None,
            convention: self.convention.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub ident: Ident,
    pub origin: Origin,
    pub r#type: Type,
}

#[derive(Debug, Clone)]
pub struct HirField {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub ident: Ident,
    pub origin: Origin,
    pub r#type: Type,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
}

#[derive(Debug, Clone)]
pub struct HirFunctionDef {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub self_mode: Option<SelfMode>,
    pub params: Vec<HirParam>,
    pub return_type: Type,
    pub body: HirBlock,
}

impl HirFunctionDef {
    pub fn function_type(&self) -> FunctionType {
        let params = self
            .params
            .iter()
            .map(|p| FunctionTypeParam::described(p.ident.clone(), p.span, p.r#type.clone()))
            .collect::<Vec<_>>();

        FunctionType {
            params,
            return_type: Box::new(self.return_type.clone()),
            is_variadic: false,
            self_mode: self.self_mode,
            convention: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirStructDef {
    pub id: HirId,
    pub span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub fields: Vec<HirField>,
    pub functions: Vec<HirFunctionDef>,
    pub is_marker: bool,
}

#[derive(Debug, Clone)]
pub struct HirUnionDef {
    pub id: HirId,
    pub span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub fields: Vec<HirField>,
    pub functions: Vec<HirFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct HirEnumDef {
    pub id: HirId,
    pub span: Span,
    pub annotations: Vec<HirAnnotation>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub header: Vec<HirField>,
    pub dynamic_fields: Vec<HirField>,
    pub variants: Vec<HirEnumVariant>,
    pub functions: Vec<HirFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub args: Vec<HirExprNode>,
    pub fields: Vec<HirField>,
}

#[derive(Debug, Clone)]
pub struct HirSpecDef {
    pub id: HirId,
    pub span: Span,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub functions: Vec<HirSpecFunction>,
    pub annotations: Vec<HirAnnotation>,
}

#[derive(Debug, Clone)]
pub struct HirSpecFunction {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub name: Ident,
    pub self_mode: Option<SelfMode>,
    pub params: Vec<HirParam>,
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<HirBlock>,
}

#[derive(Debug, Clone)]
pub struct HirGapDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub functions: Vec<HirGapFunction>,
}

#[derive(Debug, Clone)]
pub struct HirGapFunction {
    pub id: HirId,
    pub span: Span,
    pub name_span: Span,
    pub name: Ident,
    pub visibility: Visibility,
    pub params: Vec<HirParam>,
    pub return_type: Type,
}

#[derive(Debug, Clone)]
pub struct HirGlueDef {
    pub id: HirId,
    pub span: Span,
    pub gap: Path,
    pub functions: Vec<HirFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct HirConformDef {
    pub id: HirId,
    pub span: Span,
    pub generics: Vec<HirGenericParam>,
    pub target: Type,
    pub spec: Type,
    pub functions: Vec<HirFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct HirPrimitiveDef {
    pub id: HirId,
    pub span: Span,
    pub generics: Vec<HirGenericParam>,
    pub target: Type,
    pub functions: Vec<HirFunctionDef>,
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Declaration(HirDeclaration),
    DeclarationWithInit(HirDeclaration, HirExprNode),
    Expression(HirExprNode),
    Return(HirExprNode),
    WalrusDeclaration(HirWalrusDeclaration),
    While(HirWhile),
    Loop(HirLoop),
    For(HirFor),
    ForIn(HirForIn),
    Break(HirBreak),
    Continue(HirContinue),
    Defer(HirDefer),
    InlineAsm(HirInlineAsm),
}

#[derive(Debug, Clone)]
pub struct HirInlineAsm {
    pub id: HirId,
    pub span: Span,
    pub descriptors: Vec<HirAsmDescriptor>,
    pub body: String,
    pub body_span: Span,
}

#[derive(Debug, Clone)]
pub struct HirAsmDescriptor {
    pub id: HirId,
    pub span: Span,
    pub kind: HirAsmDescriptorKind,
}

#[derive(Debug, Clone)]
pub enum HirAsmDescriptorKind {
    Reg {
        expr: HirExprNode,
        physical: Option<String>,
    },
    Comp {
        name: Ident,
        origin: Origin,
    },
    Clobber {
        register: String,
    },
}

#[derive(Debug, Clone)]
pub struct HirDefer {
    pub id: HirId,
    pub span: Span,
    pub body: HirBlock,
}

#[derive(Debug, Clone)]
pub struct HirBreak {
    pub id: HirId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirContinue {
    pub id: HirId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
    pub tail: Option<Box<HirExprNode>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirWhile {
    pub id: HirId,
    pub span: Span,
    pub condition: HirExprNode,
    pub body: HirBlock,
}

#[derive(Debug, Clone)]
pub struct HirLoop {
    pub id: HirId,
    pub span: Span,
    pub body: HirBlock,
}

#[derive(Debug, Clone)]
pub struct HirFor {
    pub id: HirId,
    pub span: Span,
    pub init: Vec<HirStmt>,
    pub condition: Option<HirExprNode>,
    pub post: Option<HirExprNode>,
    pub body: HirBlock,
}

#[derive(Debug, Clone)]
pub struct HirForIn {
    pub id: HirId,
    pub span: Span,
    pub mutable: bool,
    pub binding: Ident,
    pub binding_type: Option<Type>,
    pub iterator: HirExprNode,
    pub body: HirBlock,
}

#[derive(Debug, Clone)]
pub struct HirWalrusDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub origin: Origin,
    pub value: HirExprNode,
    pub mutable: bool,
    pub comp: bool,
}

#[derive(Debug, Clone)]
pub struct HirExprNode {
    pub id: HirId,
    pub span: Span,
    /// The syntax owner of the source construct this node came from. Nodes
    /// the lowering synthesizes have no written syntax and keep the default.
    pub origin: Origin,
    pub expr: HirExpr,
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Place(HirPlace),
    Number(NumberExpr),
    String(StringExpr),
    ByteString(ByteStringExpr),
    Bool(bool),
    Char(char),
    Codeblock(HirBlock),
    If(HirIf),
    FunctionCall(HirFunctionCall),
    Assignment(HirAssignment),
    CompoundAssign(HirCompoundAssign),
    AddressOf(HirAddressOf),
    Reveal(HirReveal),
    Comp(Box<HirExprNode>),
    Negate(Box<HirExprNode>),
    BitNot(Box<HirExprNode>),
    Not(Box<HirExprNode>),
    Logical(HirLogical),
    Increment(Box<HirExprNode>),
    Decrement(Box<HirExprNode>),
    BinaryOp(HirBinaryOp),
    ArrayLiteral(Vec<HirExprNode>),
    StructLiteral(HirStructLiteral),
    Slice(HirSlice),
    Range(HirRange),
    Match(HirMatch),
    Cast(HirCast),
    Sizeof(Type),
    Try(HirTry),
}

#[derive(Debug, Clone)]
pub struct HirLogical {
    pub op: LogicalOp,
    pub left: Box<HirExprNode>,
    pub right: Box<HirExprNode>,
}

/// A postfix `base?`. Lowering is purely syntactic: whether `base` is a
/// fallible type at all is a semantic question the analyzer answers.
#[derive(Debug, Clone)]
pub struct HirTry {
    pub base: Box<HirExprNode>,
    pub operator_span: Span,
}

#[derive(Debug, Clone)]
pub struct HirCast {
    pub target: Type,
    pub base: Box<HirExprNode>,
}

#[derive(Debug, Clone)]
pub struct HirStructLiteral {
    pub path: ExprPath,
    pub fields: Vec<HirStructLiteralField>,
}

#[derive(Debug, Clone)]
pub struct HirStructLiteralField {
    pub name: Ident,
    pub name_span: Span,
    pub name_origin: Origin,
    pub value: HirExprNode,
}

#[derive(Debug, Clone)]
pub struct HirIf {
    pub branches: Vec<(HirExprNode, HirBlock)>,
    pub else_branch: Option<HirBlock>,
}

#[derive(Debug, Clone)]
pub struct HirSlice {
    pub base: HirPlace,
    pub range: HirRange,
}

#[derive(Debug, Clone)]
pub struct HirRange {
    pub start: Option<Box<HirExprNode>>,
    pub end: HirRangeEnd,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirRangeEnd {
    Inclusive(Box<HirExprNode>),
    Exclusive(Box<HirExprNode>),
    Open,
}

impl HirRangeEnd {
    pub fn expr(&self) -> Option<&HirExprNode> {
        match self {
            Self::Inclusive(e) | Self::Exclusive(e) => Some(e),
            Self::Open => None,
        }
    }

    pub fn inclusive(&self) -> bool {
        matches!(self, Self::Inclusive(_) | Self::Open)
    }
}

impl HirRange {
    pub fn is_catch_all(&self) -> bool {
        self.start.is_none() && matches!(self.end, HirRangeEnd::Open)
    }

    pub fn inclusive(&self) -> bool {
        self.end.inclusive()
    }
}

#[derive(Debug, Clone)]
pub struct HirMatch {
    pub scrutinee: Box<HirExprNode>,
    pub arms: Vec<HirMatchArm>,
    pub else_branch: Option<HirBlock>,
}

#[derive(Debug, Clone)]
pub struct HirMatchArm {
    pub pattern: HirPattern,
    pub body: HirExprNode,
    pub span: Span,
}

/// Both readings the pattern syntax allowed, carried unchanged from the AST.
/// Choosing between them needs the scrutinee's resolved type, so it is the
/// analyzer's decision, not lowering's.
#[derive(Debug, Clone)]
pub struct HirPattern {
    pub value: Option<HirPatternValue>,
    pub r#type: Option<Type>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirPatternValue {
    Value(HirExprNode),
    Range(HirRange),
}

impl HirPattern {
    pub fn span(&self) -> Span {
        self.span
    }

    pub fn catch_all_range(&self) -> Option<&HirRange> {
        match self.value.as_ref()? {
            HirPatternValue::Range(range) if range.is_catch_all() => Some(range),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirPlace {
    pub root: HirPlaceRoot,
    pub projections: Vec<HirProjection>,
}

#[derive(Debug, Clone)]
pub enum HirPlaceRoot {
    Path(ExprPath),
    Expr(Box<HirExprNode>),
}

#[derive(Debug, Clone)]
pub enum HirProjection {
    FieldAccess(Ident, Origin),
    Index(Box<HirExprNode>),
    Deref,
}

#[derive(Debug, Clone)]
pub struct HirFunctionCall {
    pub callee: Box<HirExprNode>,
    pub args: Vec<HirExprNode>,
}

#[derive(Debug, Clone)]
pub struct HirBinaryOp {
    pub op: BinaryOp,
    pub left: Box<HirExprNode>,
    pub right: Box<HirExprNode>,
}

#[derive(Debug, Clone)]
pub struct HirAddressOf {
    pub base: Box<HirExprNode>,
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct HirReveal {
    pub base: Box<HirExprNode>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct HirAssignment {
    pub target: Box<HirExprNode>,
    pub value: Box<HirExprNode>,
}

#[derive(Debug, Clone)]
pub struct HirCompoundAssign {
    pub target: Box<HirExprNode>,
    pub op: BinaryOp,
    pub value: Box<HirExprNode>,
}
