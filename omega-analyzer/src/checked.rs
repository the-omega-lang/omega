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
    /// Guaranteed by `Analyzer::check_function_return` to either end in a
    /// tail expression whose type matches `return_type`, or end in a
    /// statement-level `return` -- codegen relies on this to know it never
    /// has to fall off the end of a non-`Void` function.
    pub body: CheckedBlock,
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
    While(CheckedWhile),
    /// Boxed: `CheckedFor` alone is by far the largest variant here (it
    /// embeds a whole `CheckedBlock` for its body plus another for `init`'s
    /// contribution), and would otherwise force every `CheckedStmt` -- most
    /// of which are tiny -- to be sized for the rare large one.
    For(Box<CheckedFor>),
}

/// A `{ ... }` block's statements plus its optional final expression (no
/// trailing `;`), which is the block's own value. Shared by bare `{}`
/// expressions, `if`/`else` branches, `while`/`for` bodies, and function
/// bodies -- see `Analyzer::analyze_block`, which builds one uniformly for
/// all of them, and `Analyzer::block_type`, which reads its effective type
/// back out (`None` if it ends in a `return`, meaning "diverges, compatible
/// with anything" -- the same reasoning behind Rust's `!` type, without a
/// dedicated `ResolvedType` for it).
#[derive(Debug, Clone)]
pub struct CheckedBlock {
    pub stmts: Vec<CheckedStmt>,
    pub tail: Option<Box<CheckedExprNode>>,
}

/// `while cond { body }` -- `condition` is guaranteed `Bool`.
#[derive(Debug, Clone)]
pub struct CheckedWhile {
    pub condition: CheckedExprNode,
    pub body: CheckedBlock,
}

/// `for init; cond; post { body }` -- unlike the parser's `HirFor`,
/// `condition` here is *not* optional: analysis rejects a `for` loop with no
/// condition (`AnalysisErrorKind::ForLoopMissingCondition`) rather than
/// treating an omitted one as "always true," since an always-true condition
/// with no `break`/`continue` in this language would make the loop's exit
/// block provably unreachable -- a soundness problem for codegen (cranelift
/// requires every block to end in a terminator, and there would be nothing
/// to ever jump into that one), not just a style choice. `init`/`post` stay
/// optional; neither affects reachability the way a missing condition does.
#[derive(Debug, Clone)]
pub struct CheckedFor {
    pub init: Vec<CheckedStmt>,
    pub condition: CheckedExprNode,
    pub post: Option<CheckedExprNode>,
    pub body: CheckedBlock,
}

#[derive(Debug, Clone)]
pub struct CheckedExprNode {
    pub id: HirId,
    pub span: SimpleSpan,
    pub r#type: ResolvedType,
    pub kind: CheckedExpr,
}

/// A number literal's already-parsed value, in the widest container that can
/// hold any value of its kind -- the exact width/signedness to narrow it to
/// when emitting IR comes from the node's own `r#type` (see
/// `CheckedExprNode::r#type`), which analysis has already range-checked the
/// value against, so codegen only ever narrows losslessly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumberValue {
    Signed(i64),
    Unsigned(u64),
    Float(f64),
}

#[derive(Debug, Clone)]
pub enum CheckedExpr {
    Place(CheckedPlace),
    /// The literal's value, already parsed and range-checked against its
    /// resolved type by analysis -- codegen never re-parses source text.
    Number(NumberValue),
    Bool(bool),
    /// A single Unicode scalar value (`ResolvedType::Char`) -- kept as a
    /// `char`, not pre-converted to its `u32` codepoint, since it's still
    /// meaningful source-level data until codegen actually needs the bits.
    Char(char),
    String(String),
    FunctionCall(CheckedFunctionCall),
    Assignment(CheckedAssignment),
    AddressOf(CheckedAddressOf),
    Negate(Box<CheckedExprNode>),
    /// `++base`/`--base` never survives past analysis as its own node --
    /// `Analyzer::analyze_incr_decr` desugars it directly into an ordinary
    /// `Assignment` of `base + 1`/`base - 1` (a `BinaryOp` over `base`'s own
    /// place and a `Number` matching its exact resolved type), so codegen
    /// needs no dedicated increment/decrement machinery at all.
    BinaryOp(CheckedBinaryOp),
    /// A bare `{ ... }` used as an expression -- its value is its tail
    /// expression (`Void` if it has none).
    Codeblock(CheckedBlock),
    /// `if cond { ... } else if cond { ... } else { ... }` -- every branch
    /// (and `else_branch`, if present) is guaranteed to agree on this node's
    /// own `r#type` (see `Analyzer`'s `HirExpr::If` arm), except for a
    /// branch that diverges (ends in `return`), which is exempt the same
    /// way `CheckedBlock`'s tail-less-but-terminates-in-`return` case is.
    If(CheckedIf),
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

/// Both operands are guaranteed to share the same numeric resolved type by
/// the time this is constructed -- codegen never re-checks it, and picks
/// the concrete instruction (`iadd` vs `fadd`, `sdiv` vs `udiv`, ...) from
/// that shared type. For a comparison op (`op.is_comparison()`), this
/// node's own type is always `Bool` regardless of the (still-numeric,
/// still-matching) operand type; for an arithmetic op, this node's type is
/// the same as the operands'.
#[derive(Debug, Clone)]
pub struct CheckedBinaryOp {
    pub op: BinaryOp,
    pub left: Box<CheckedExprNode>,
    pub right: Box<CheckedExprNode>,
}

/// See `CheckedExpr::If`'s doc comment.
#[derive(Debug, Clone)]
pub struct CheckedIf {
    pub branches: Vec<(CheckedExprNode, CheckedBlock)>,
    pub else_branch: Option<CheckedBlock>,
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
