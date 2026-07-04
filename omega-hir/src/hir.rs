use crate::ids::HirId;
use omega_parser::prelude::{FunctionType, Ident, NumberExpr, SimpleSpan, StringExpr, Type};

#[derive(Debug, Clone)]
pub struct HirModule {
    pub id: crate::ids::ModuleId,
    pub items: Vec<HirItem>,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Declaration(HirDeclaration),
    ExternDeclaration(HirExternDeclaration),
    FunctionDefinition(HirFunctionDef),
    Struct(HirStructDef),
}

#[derive(Debug, Clone)]
pub struct HirDeclaration {
    pub id: HirId,
    pub span: SimpleSpan,
    pub ident: Ident,
    pub r#type: Type,
}

#[derive(Debug, Clone)]
pub struct HirExternDeclaration {
    pub id: HirId,
    pub span: SimpleSpan,
    pub ident: Ident,
    pub r#type: Type,
}

/// A function parameter or struct field -- structurally identical (a named,
/// typed declaration slot), and both are self-identifying like every other
/// declaration-shaped HIR node, unlike the raw `DeclarationStmt` they used to
/// be lowered as verbatim (which had no id of its own).
#[derive(Debug, Clone)]
pub struct HirParam {
    pub id: HirId,
    pub span: SimpleSpan,
    pub ident: Ident,
    pub r#type: Type,
}

/// A function definition, used identically whether it's a top-level item or
/// a struct method (`HirStructDef::functions`) -- both are self-identifying,
/// so there's no special-cased id-minting for methods like there used to be
/// in the parser.
#[derive(Debug, Clone)]
pub struct HirFunctionDef {
    pub id: HirId,
    pub span: SimpleSpan,
    pub name: Ident,
    pub is_member_function: bool,
    /// For member functions, the synthetic `self: *StructName` parameter is
    /// already inserted here by lowering -- downstream consumers never need
    /// to special-case it.
    pub params: Vec<HirParam>,
    pub return_type: Type,
    pub body: Vec<HirStmt>,
}

impl HirFunctionDef {
    pub fn function_type(&self) -> FunctionType {
        let params = self
            .params
            .iter()
            .map(|p| (p.ident.clone(), p.r#type.clone()))
            .collect::<Vec<_>>();

        FunctionType {
            params,
            return_type: Box::new(self.return_type.clone()),
            is_variadic: false,
            is_member_function: self.is_member_function,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirStructDef {
    pub id: HirId,
    pub span: SimpleSpan,
    pub name: Ident,
    pub fields: Vec<HirParam>,
    pub functions: Vec<HirFunctionDef>,
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Declaration(HirDeclaration),
    ExternDeclaration(HirExternDeclaration),
    Expression(HirExprNode),
    Return(HirExprNode),
    Struct(HirStructDef),
}

#[derive(Debug, Clone)]
pub struct HirExprNode {
    pub id: HirId,
    pub span: SimpleSpan,
    pub expr: HirExpr,
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Place(HirPlace),
    Number(NumberExpr),
    String(StringExpr),
    Codeblock(Vec<HirStmt>),
    FunctionCall(HirFunctionCall),
    Assignment(HirAssignment),
    AddressOf(HirAddressOf),
}

/// The parser has no notion of "places"/lvalues -- it only knows `Ident`,
/// `FieldAccess`, `Index`, and `Deref` as plain expression-forming
/// constructs (see `omega_parser::syntax::expression`). Lowering is what
/// recognizes a chain of those as denoting an addressable location and
/// flattens it into this single shape: a root plus zero or more
/// projections, in source order. A bare identifier is just a place with no
/// projections.
#[derive(Debug, Clone)]
pub struct HirPlace {
    pub root: HirPlaceRoot,
    pub projections: Vec<HirProjection>,
}

#[derive(Debug, Clone)]
pub enum HirPlaceRoot {
    Ident(Ident),
    /// The base of a projection chain that isn't a bare identifier, e.g.
    /// `foo().bar` -- the root is the `foo()` call expression.
    Expr(Box<HirExprNode>),
}

#[derive(Debug, Clone)]
pub enum HirProjection {
    FieldAccess(Ident),
    Index(Box<HirExprNode>),
    /// `*expr` as part of a place chain (e.g. `*ptr`, `(*ptr).field`) --
    /// whether the pointer type is valid here is a question for analysis,
    /// not lowering.
    Deref,
}

#[derive(Debug, Clone)]
pub struct HirFunctionCall {
    pub callee: Box<HirExprNode>,
    pub args: Vec<HirExprNode>,
}

/// `&base` -- unlike `Deref`, this never denotes a place itself (it produces
/// a pointer *value*). `base` is lowered as an ordinary expression, not
/// structurally guaranteed to be a place at this level -- same treatment as
/// `HirAssignment.target`, validated during analysis.
#[derive(Debug, Clone)]
pub struct HirAddressOf {
    pub base: Box<HirExprNode>,
}

/// `target` is deliberately not typed as `HirPlace`: the parser doesn't
/// guarantee an assignment's left-hand side is syntactically a place (e.g.
/// `5 = 3` parses fine), so that's still validated downstream in semantic
/// analysis, same as before this change.
#[derive(Debug, Clone)]
pub struct HirAssignment {
    pub target: Box<HirExprNode>,
    pub value: Box<HirExprNode>,
}
