//! A function's control-flow graph -- the whole reason this crate exists.
//! `CheckedBlock`/`CheckedStmt`'s `if`/`match`/`while`/`for`/`break`/
//! `continue`/`return`/`defer` are all expression- or statement-shaped in
//! the checked tree; here they're already flattened into an explicit graph
//! of [`MirBlockData`]s ending in one [`MirTerminator`] each, exactly the
//! shape both Cranelift and (eventually) an LLVM backend want. See
//! `docs/16-mir-and-codegen.md` for the full rationale and worked
//! examples of how each construct lowers.
//!
//! What's *not* flattened: ordinary computation (arithmetic, calls, casts,
//! aggregates, place projections) stays a tree ([`MirExpr`]), deliberately
//! not reduced to three-address form -- see the module doc comment's
//! sibling note in the design docs for why. A [`MirStatement`] is just one
//! such tree, evaluated for its side effects with its value discarded.

use crate::ids::{BlockId, LocalId};
use omega_analyzer::checked::{CastKind, NumberValue};
use omega_analyzer::resolved_type::{ConstValue, ResolvedFunctionType, ResolvedType};
use omega_hir::{BinaryOp, HirId};
use omega_parser::prelude::{Ident, Span};

/// One function's whole control-flow graph plus every local it declares.
#[derive(Debug, Clone)]
pub struct MirBody {
    /// `0..arg_count` are the function's own parameters, in declaration
    /// order; everything from `arg_count` onward is either a user-declared
    /// local (`source: Some`) or a lowering-synthesized temporary --
    /// today, only a `defer`'s own flag (`source: None`). Unified into one
    /// unified index space (rather than a separate `Storage::Parameter`
    /// case, as `CheckedPlaceRoot` has) because codegen treats both
    /// identically except for *where* their initial value comes from: a
    /// parameter is seeded from the entry block's own Cranelift block
    /// params, a declared/synthetic local gets a stack slot -- a single
    /// `local_id < arg_count` check, not two data shapes.
    pub locals: Vec<MirLocalDecl>,
    pub arg_count: usize,
    /// Block `0` is always this body's entry block. Built by
    /// `crate::lower::function::FunctionLowerer`, which mints `BlockId`s
    /// sequentially as it walks the checked body.
    pub blocks: Vec<MirBlockData>,
}

#[derive(Debug, Clone)]
pub struct MirLocalDecl {
    /// `Some` for a user-declared variable or parameter -- the `HirId` its
    /// `CheckedDeclaration`/`CheckedParam` carried, kept for diagnostics and
    /// for `crate::lower::place`'s `HirId -> LocalId` lookup table. `None`
    /// for a lowering-synthesized temporary (a `defer` flag today).
    pub source: Option<HirId>,
    pub r#type: ResolvedType,
}

/// One basic block: a straight-line run of statements, ending in exactly
/// one terminator.
///
/// Deliberately *not* Cranelift-style block parameters (a phi-equivalent):
/// an earlier version of this design threaded an `if`/`match` join's value
/// that way, but it silently breaks the moment a *sibling* expression (say,
/// the other operand of the same enclosing `BinaryOp`) builds further
/// blocks before that value is actually consumed -- the value ends up
/// referenced from whatever block the enclosing statement/terminator
/// finally lands in, which is no longer the block that produced it, and a
/// Cranelift block's own params are only ever valid for a use gated behind
/// dominance from that exact block, not "wherever lowering happens to be
/// by the time the tree gets consumed." So every cross-block value in this
/// MIR -- an `if`/`match` join's result, the function's own return-value
/// threaded through its `defer` exit chain -- is an ordinary synthetic
/// [`LocalId`] instead (see `MirPlaceRoot::Local`), written by an ordinary
/// `Assign` statement in each producing block and read back by an ordinary
/// `Place` wherever it's needed, exactly like Rust's own MIR does (no
/// block-argument mechanism there either) -- a block never carries any
/// incoming value of its own.
#[derive(Debug, Clone)]
pub struct MirBlockData {
    /// Evaluated in order, purely for side effects -- each is a whole
    /// `MirExprNode` tree (an assignment, a call, ...), never a value this
    /// block itself consumes (that's what `terminator`'s own operand(s) are
    /// for).
    pub statements: Vec<MirExprNode>,
    pub terminator: MirTerminator,
}

#[derive(Debug, Clone)]
pub enum MirTerminator {
    /// Unconditional jump -- carries no value (see `MirBlockData`'s doc
    /// comment for why).
    Goto(BlockId),
    /// A single two-way test -- `if`/`while`/`for`'s own condition, and one
    /// bound of a `match` arm's (possibly multi-bound) pattern test. A
    /// multi-condition arm is nested `Branch`es chained by lowering, exactly
    /// like today's `emit_match`'s nested `brif` chain -- there is no
    /// multi-way/jump-table terminator (yet).
    Branch { condition: MirExprNode, then_block: BlockId, else_block: BlockId },
    /// The function's one real return, sitting at the end of its exit
    /// chain (see `MirBlockData`'s doc comment) -- never emitted directly
    /// at a nested `return`'s own position; those always `Goto` the chain
    /// instead.
    Return(Option<MirExprNode>),
    /// A provably-unreachable point, e.g. an exhaustive `match` with no
    /// `else` falling off the end -- codegen traps here, exactly like
    /// today's `emit_match`'s `TrapCode` use.
    Unreachable,
}

#[derive(Debug, Clone)]
pub struct MirExprNode {
    pub id: HirId,
    pub span: Span,
    pub r#type: ResolvedType,
    pub kind: MirExpr,
}

/// `CheckedExpr`'s direct analogue, minus `If`/`Match`/`Codeblock` -- those
/// three are exactly the variants that become [`MirBlockData`] graphs
/// instead of expression nodes (see the module doc comment).
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
    ConstSlice(ConstValue),
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

/// One `field_index`-tagged initializer -- shared shape for a struct
/// literal's own fields and an enum variant's body fields, same as
/// `CheckedStructLiteralField` covers both today.
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
}

#[derive(Debug, Clone)]
pub enum MirPlaceRoot {
    /// Covers both a function parameter and a declared local uniformly --
    /// see `MirBody::locals`'s doc comment.
    Local { id: LocalId, r#type: ResolvedType },
    /// A named function, resolved to a callable symbol -- never itself
    /// further-projected, same invariant `Storage::Function` documents
    /// today.
    Function(HirId),
    /// A top-level variable or non-function extern -- storage layout is
    /// still undecided (`todo!()` in codegen), same as `Storage::Global`
    /// today.
    Global { id: HirId, r#type: ResolvedType },
    /// The base of a projection chain that isn't a bare name, e.g.
    /// `foo().bar` -- the root is the `foo()` call expression.
    Expr(Box<MirExprNode>),
}

/// `CheckedProjection`'s direct analogue.
#[derive(Debug, Clone)]
pub enum MirProjection {
    FieldAccess { field: Ident, index: usize, r#type: ResolvedType },
    Index { index_expr: Box<MirExprNode>, item_type: ResolvedType },
    Deref { r#type: ResolvedType },
    SliceLength,
    EnumTag { r#type: ResolvedType },
    EnumHeader { field: Ident, index: usize, r#type: ResolvedType },
    EnumDynamicField { field: Ident, index: usize, r#type: ResolvedType },
    EnumBody { variant_index: usize, field_index: usize, r#type: ResolvedType },
    UnionField { field: Ident, index: usize, r#type: ResolvedType },
}
