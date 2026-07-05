use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_hir::{HirId, ModuleId};
use omega_parser::prelude::{BinaryOp, Ident, SimpleSpan};

/// The output of semantic analysis: a fully resolved and verified tree, not a
/// side-table report. By the time a `CheckedModule` exists, every
/// enforcement point (assignment targets are places, types match, fields and
/// indices exist, names aren't redeclared) has already been settled -- so
/// codegen can synthesize IR by pure structural recursion with no
/// re-validation of anything checked here.
#[derive(Debug, Clone)]
pub struct CheckedModule {
    pub id: ModuleId,
    pub items: Vec<CheckedItem>,
}

#[derive(Debug, Clone)]
pub enum CheckedItem {
    /// A top-level `ident: type;` with no initializer syntax -- global data
    /// storage isn't decided yet (no linkage/section/zero-init story), so
    /// this is resolved and type-checked like everything else, but codegen
    /// still has nothing sound to do with it (`todo!()`).
    Declaration(CheckedDeclaration),
    ExternDeclaration(CheckedExternDecl),
    FunctionDefinition(CheckedFunctionDef),
    Struct(CheckedStructDef),
}

/// Where a resolved variable reference's value physically lives. Attached
/// only to *references* (`CheckedPlaceRoot::Variable`), not to declarations
/// themselves -- which storage a declaration gets is implied by which
/// checked node produced it (a `CheckedStmt::Declaration` is always `Local`,
/// a `CheckedParam` is always `Parameter`, a function is always `Function`, a
/// top-level `CheckedDeclaration`/data extern is `Global`). Carrying this
/// inline at the use site is what lets codegen trace a declaration back to
/// its storage without re-deriving it from a scope walk on every access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// A stack-resident local variable, declared inside a function body.
    Local,
    /// A function parameter, materialized as SSA value(s) at function entry.
    Parameter,
    /// A named function -- top-level, extern, or a struct method -- resolved
    /// to a callable symbol.
    Function,
    /// A top-level variable or non-function extern; storage layout for this
    /// is not yet decided (`todo!()` in codegen).
    Global,
}

#[derive(Debug, Clone)]
pub struct CheckedDeclaration {
    pub id: HirId,
    pub span: SimpleSpan,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedExternDecl {
    pub id: HirId,
    pub span: SimpleSpan,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedParam {
    pub id: HirId,
    pub span: SimpleSpan,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedFunctionDef {
    pub id: HirId,
    pub span: SimpleSpan,
    pub name: Ident,
    pub is_member_function: bool,
    pub is_variadic: bool,
    pub params: Vec<CheckedParam>,
    pub return_type: ResolvedType,
    pub body: Vec<CheckedStmt>,
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
            is_member_function: self.is_member_function,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckedStructDef {
    pub id: HirId,
    pub span: SimpleSpan,
    pub name: Ident,
    pub fields: Vec<CheckedParam>,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub enum CheckedStmt {
    Declaration(CheckedDeclaration),
    ExternDeclaration(CheckedExternDecl),
    Expression(CheckedExprNode),
    Return(CheckedExprNode),
    Struct(CheckedStructDef),
}

#[derive(Debug, Clone)]
pub struct CheckedExprNode {
    pub id: HirId,
    pub span: SimpleSpan,
    pub r#type: ResolvedType,
    pub kind: CheckedExpr,
}

#[derive(Debug, Clone)]
pub enum CheckedExpr {
    Place(CheckedPlace),
    /// The literal's value, already parsed and range-checked against its
    /// resolved type by analysis -- codegen never re-parses source text.
    Number(i32),
    String(String),
    FunctionCall(CheckedFunctionCall),
    Assignment(CheckedAssignment),
    AddressOf(CheckedAddressOf),
    Negate(Box<CheckedExprNode>),
    BinaryOp(CheckedBinaryOp),
    /// Block-expressions have no decided value/placement semantics yet (the
    /// grammar has no tail-expression-without-`;` to give one a value) --
    /// still walked and scope-checked by analysis for soundness, but codegen
    /// has nothing sound to emit for it yet (`todo!()`).
    Codeblock(Vec<CheckedStmt>),
    /// Elements are guaranteed to all share `item_type` by the time this is
    /// constructed -- codegen never re-checks it. The literal's own type is
    /// `ResolvedType::SizedArray(item_type, elements.len())`.
    ArrayLiteral(CheckedArrayLiteral),
    Slice(CheckedSlice),
}

#[derive(Debug, Clone)]
pub struct CheckedArrayLiteral {
    pub item_type: ResolvedType,
    pub elements: Vec<CheckedExprNode>,
}

/// `base[start..end]` -- `base`'s resolved type is guaranteed to be
/// `SizedArray` or `Slice` (never anything else) by the time this is
/// constructed, and `start`/`end` (when present) are guaranteed `I32`.
/// `item_type` is `base`'s element type, carried the same way
/// `CheckedProjection::Index`'s is, so codegen never has to re-derive it
/// from `base`'s type.
#[derive(Debug, Clone)]
pub struct CheckedSlice {
    pub base: CheckedPlace,
    pub item_type: ResolvedType,
    pub start: Option<Box<CheckedExprNode>>,
    pub end: Option<Box<CheckedExprNode>>,
}

#[derive(Debug, Clone)]
pub struct CheckedPlace {
    pub root: CheckedPlaceRoot,
    pub projections: Vec<CheckedProjection>,
}

#[derive(Debug, Clone)]
pub enum CheckedPlaceRoot {
    Variable {
        decl_id: HirId,
        storage: Storage,
        r#type: ResolvedType,
    },
    /// The base of a projection chain that isn't a bare name, e.g.
    /// `foo().bar` -- the root is the `foo()` call expression.
    Expr(Box<CheckedExprNode>),
}

#[derive(Debug, Clone)]
pub enum CheckedProjection {
    /// `index`/`r#type` are the field's resolved position and type within
    /// the base struct, already looked up by name during analysis -- codegen
    /// slices straight into the field list by index, no name search, no
    /// "field doesn't exist" failure mode left to hit.
    FieldAccess {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    Index {
        index_expr: Box<CheckedExprNode>,
        item_type: ResolvedType,
    },
    /// `*expr` (explicit), or a seamless one-level pointer-to-struct
    /// autoderef inserted by analysis before a `FieldAccess` projection --
    /// `r#type` is the pointee type.
    Deref {
        r#type: ResolvedType,
    },
    /// `slice.length` -- reads a `Slice`'s length component. A dedicated
    /// projection rather than a `FieldAccess` variant, since a slice isn't a
    /// `Struct` and `length` isn't a real field looked up by name/index; see
    /// `Analyzer::resolve_field_projection`'s special case.
    SliceLength,
}

#[derive(Debug, Clone)]
pub struct CheckedFunctionCall {
    pub callee: Box<CheckedExprNode>,
    pub fn_type: ResolvedFunctionType,
    pub args: Vec<CheckedExprNode>,
}

/// Both operands are guaranteed `ResolvedType::I32` (the only numeric type
/// today) by the time this is constructed -- codegen never re-checks it.
#[derive(Debug, Clone)]
pub struct CheckedBinaryOp {
    pub op: BinaryOp,
    pub left: Box<CheckedExprNode>,
    pub right: Box<CheckedExprNode>,
}

/// `target` is a `CheckedPlace`, not a general expression: analysis rejects
/// (`AssignmentTargetNotAPlace`) any assignment whose left-hand side isn't
/// syntactically a place before a `CheckedAssignment` is ever constructed, so
/// this is an enforced invariant of the type, not a convention codegen has to
/// trust.
#[derive(Debug, Clone)]
pub struct CheckedAssignment {
    pub target: CheckedPlace,
    pub value: Box<CheckedExprNode>,
}

/// `place` is a `CheckedPlace`, not a general expression, for the same
/// reason `CheckedAssignment.target` is: analysis rejects
/// (`AddressOfNotAPlace`) any `&expr` whose operand isn't syntactically a
/// place before this is ever constructed.
#[derive(Debug, Clone)]
pub struct CheckedAddressOf {
    pub place: CheckedPlace,
}
