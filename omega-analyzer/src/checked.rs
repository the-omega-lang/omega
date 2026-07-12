use crate::resolved_type::{ConstValue, ResolvedFunctionType, ResolvedType};
use omega_hir::{HirId, ModuleId};
use omega_parser::prelude::{BinaryOp, Ident, Span};

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
    ExternDeclaration(CheckedExternDeclaration),
    FunctionDefinition(CheckedFunctionDef),
    Struct(CheckedStructDef),
    Enum(CheckedEnumDef),
    Union(CheckedUnionDef),
}

/// An extern-owned function/method a compilation actually referenced --
/// `omega_driver::Driver::collect_extern_functions`'s output, and
/// `omega_codegen::Codegen`'s input for declaring (never defining) a link
/// against it. Lives here, in `omega-analyzer`, rather than in
/// `omega-driver` (which constructs it) or `omega-codegen` (which consumes
/// it) alone, since neither of those crates depends on the other -- the
/// same reason `CheckedModule`/`CheckedItem` live here instead of in
/// either.
#[derive(Debug, Clone)]
pub struct ExternFunctionRef {
    pub decl_id: HirId,
    pub module_path: Vec<Ident>,
    pub kind: ExternFunctionKind,
    pub fn_type: ResolvedFunctionType,
}

#[derive(Debug, Clone)]
pub enum ExternFunctionKind {
    /// A top-level function, named directly.
    Free(Ident),
    /// A struct/enum/union method -- `type_name` is the owning type's own
    /// name (needed alongside `module_path` for the mangled method symbol,
    /// which is shaped differently from a free function's).
    Method { type_name: Ident, method_name: Ident },
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
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedExternDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct CheckedParam {
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
    pub span: Span,
    pub name: Ident,
    pub fields: Vec<CheckedParam>,
    pub functions: Vec<CheckedFunctionDef>,
}

/// A checked union definition -- same shape as `CheckedStructDef`; field
/// overlap is entirely a codegen-layout concern, not a checked-tree one.
#[derive(Debug, Clone)]
pub struct CheckedUnionDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub fields: Vec<CheckedParam>,
    pub functions: Vec<CheckedFunctionDef>,
}

/// A checked enum definition. Deliberately *only* the functions: the
/// tag/header/variant data codegen needs at every construction and
/// field-access site travels inside `ResolvedType::Enum`'s shared cell (on
/// the expressions themselves), so carrying a second copy here would just
/// be a divergence risk. What's left is exactly what has a compiled
/// artifact of its own -- the methods.
#[derive(Debug, Clone)]
pub struct CheckedEnumDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub functions: Vec<CheckedFunctionDef>,
}

#[derive(Debug, Clone)]
pub enum CheckedStmt {
    Declaration(CheckedDeclaration),
    ExternDeclaration(CheckedExternDeclaration),
    Expression(CheckedExprNode),
    Return(CheckedExprNode),
    While(CheckedWhile),
    /// Boxed: `CheckedFor` alone is by far the largest variant here (it
    /// embeds a whole `CheckedBlock` for its body plus another for `init`'s
    /// contribution), and would otherwise force every `CheckedStmt` -- most
    /// of which are tiny -- to be sized for the rare large one.
    For(Box<CheckedFor>),
    Break(CheckedBreak),
    Continue(CheckedContinue),
    Defer(CheckedDefer),
}

/// `defer <statement>;` / `defer { ... }` -- see `omega_hir::hir::HirDefer`'s
/// doc comment for the full semantics. `body` is checked as an ordinary
/// block (with `Analyzer::in_defer_body` set, rejecting `return`/nested
/// `defer` inside it); codegen never runs it inline at this position --
/// only in the enclosing function's shared epilogue, guarded by a runtime
/// flag set right here (see `Codegen`'s `defer_flags`/`defer_bodies`).
#[derive(Debug, Clone)]
pub struct CheckedDefer {
    pub id: HirId,
    pub span: Span,
    pub body: CheckedBlock,
}

/// `break;` -- `loop_id` is the enclosing loop's own `HirId` (from its
/// `HirWhile`/`HirFor`), already resolved by analysis (see `Analyzer`'s
/// `loop_stack`) to whichever loop this targets -- today always the
/// innermost one, but codegen looks it up by id rather than assuming "the
/// current loop," precisely so a future labeled `break 'outer;` only needs
/// analysis's resolution rule to change (search the stack for a matching
/// label instead of always taking the top), not codegen.
#[derive(Debug, Clone)]
pub struct CheckedBreak {
    pub id: HirId,
    pub span: Span,
    pub loop_id: HirId,
}

/// `continue;` -- see `CheckedBreak`.
#[derive(Debug, Clone)]
pub struct CheckedContinue {
    pub id: HirId,
    pub span: Span,
    pub loop_id: HirId,
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

/// `while cond { body }` -- `condition` is guaranteed `Bool`. `id` is what
/// `CheckedBreak`/`CheckedContinue.loop_id` refers back to when this loop is
/// their target.
#[derive(Debug, Clone)]
pub struct CheckedWhile {
    pub id: HirId,
    pub span: Span,
    pub condition: CheckedExprNode,
    pub body: CheckedBlock,
}

/// `for init; cond; post { body }` -- unlike the parser's `HirFor`,
/// `condition` here is *not* optional: analysis rejects a `for` loop with no
/// condition (`AnalysisErrorKind::ForLoopMissingCondition`) rather than
/// treating an omitted one as "always true" -- this language has no
/// constant-condition reasoning to prove such a loop's exit is ever actually
/// reached (even with `break` now available, *some* path has to reach it),
/// so requiring a real condition is what currently guarantees the exit
/// block is a valid jump target for codegen (cranelift requires every block
/// to end in a terminator). `init`/`post` stay optional; neither affects
/// reachability the way a missing condition does. `id` is what
/// `CheckedBreak`/`CheckedContinue.loop_id` refers back to when this loop is
/// their target.
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
    /// `b"..."` -- see `Expression::ByteString`'s doc comment. Its
    /// resolved type (`r#type` on the enclosing `CheckedExprNode`) is
    /// always `ResolvedType::Slice { item: U8, .. }`, unlike `String`'s
    /// always-`Pointer` type.
    ByteString(String),
    FunctionCall(CheckedFunctionCall),
    Assignment(CheckedAssignment),
    AddressOf(CheckedAddressOf),
    Negate(Box<CheckedExprNode>),
    /// `~base` -- see `Expression::BitNot`'s doc comment.
    BitNot(Box<CheckedExprNode>),
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
    /// `Name { field = value; ... }` -- the node's own `r#type` is always the
    /// struct being built (`ResolvedType::Struct`); see
    /// `CheckedStructLiteral`'s doc comment for the field guarantees.
    StructLiteral(CheckedStructLiteral),
    /// `Enum::Variant` / `Enum::Variant { field = value; ... }` -- builds a
    /// whole enum value. The node's own `r#type` is always the enum with
    /// this exact variant statically known
    /// (`ResolvedType::Enum { variant: Some(variant_index) }`); the tag and
    /// header constants come from the enum's shared cell, so only the
    /// variant's own body fields are carried here (with the same
    /// exactly-once/-typed guarantees `CheckedStructLiteral` documents,
    /// against the variant's field list).
    EnumConstruct(CheckedEnumConstruct),
    Slice(CheckedSlice),
    /// `match scrutinee { pattern => body, ... } else { ... }` -- an
    /// exhaustive switch, and (for an enum scrutinee) the proof mechanism
    /// behind sum-type subtyping: each arm's `body` is analyzed with the
    /// scrutinee's own binding narrowed to exactly the variant that arm's
    /// pattern proved (see `Analyzer::analyze_match`), so this node itself
    /// carries no narrowing information at all -- it's already baked into
    /// each arm's `CheckedMatchArm.body` as ordinary (already-refined)
    /// `ResolvedType`s on whatever place expressions appear there. Every
    /// arm (and `else_branch`, if present) is guaranteed to agree on this
    /// node's own `r#type`, exactly like `CheckedExpr::If`.
    Match(CheckedMatch),
    /// `<Type>base` -- `target_type` is this node's own `r#type` too
    /// (carried here as well since `CheckedCast` is also useful standalone
    /// in codegen's dispatch); `kind` is exactly which conversion
    /// (`Analyzer::resolve_cast_kind`) `base`'s value needs to become it.
    Cast(CheckedCast),
    /// `Union { field = value; }` -- builds a whole union value by writing
    /// exactly one field; analysis guarantees exactly one initializer was
    /// given (see `AnalysisErrorKind::UnionLiteralMissingField`/
    /// `UnionLiteralTooManyFields`). The node's own `r#type` is always the
    /// union being built (`ResolvedType::Union`).
    UnionConstruct(CheckedUnionConstruct),
    /// `&[...]` -- a compile-time slice literal (see `ConstValue::Slice`).
    /// The node's own `r#type` is always `ResolvedType::Slice { item,
    /// mutable: false }`, which already fully describes the element type,
    /// so nothing else needs to be carried here.
    ConstSlice(ConstValue),
}

/// See `CheckedExpr::UnionConstruct`. `field_index` is the field's position
/// in the union's own field list, exactly like `CheckedProjection::UnionField`
/// -- codegen zeroes the union's storage, then stores `value`'s leaves at
/// offset 0 (no tag/header, unlike `CheckedEnumConstruct`).
#[derive(Debug, Clone)]
pub struct CheckedUnionConstruct {
    pub field_index: usize,
    pub value: Box<CheckedExprNode>,
}

/// See `CheckedExpr::Cast`. Every castable type flattens to exactly one IR
/// leaf (numeric or pointer), so this never touches the multi-leaf
/// flattening machinery at all -- codegen is a single instruction (or none,
/// for `CastKind::Reinterpret`) applied to `base`'s own one leaf.
#[derive(Debug, Clone)]
pub struct CheckedCast {
    pub kind: CastKind,
    pub target_type: ResolvedType,
    pub base: Box<CheckedExprNode>,
}

/// Exactly which conversion a cast needs, already resolved from both
/// sides' `ResolvedType::cast_class` -- codegen never re-derives this, it
/// just picks the one Cranelift instruction (or none) each variant maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    /// No instruction needed at all -- source and target already share the
    /// same underlying IR representation: same-width int-family (regardless
    /// of Omega-level signedness -- Cranelift has no separate signed/
    /// unsigned integer *types*, only signed/unsigned *operations*, and a
    /// pointer uses that same width-64 type too), or identical float width.
    Reinterpret,
    IntExtend { signed: bool },
    IntTruncate,
    IntToFloat { signed: bool },
    /// Saturating, not trapping (`fcvt_to_*_sat`, not `fcvt_to_*`) --
    /// matches Rust's own `as` behavior; a numeric cast shouldn't be a
    /// surprise trap source over an out-of-range or NaN value.
    FloatToInt { signed: bool },
    FloatExtend,
    FloatTruncate,
}

/// See `CheckedExpr::EnumConstruct`.
#[derive(Debug, Clone)]
pub struct CheckedEnumConstruct {
    pub variant_index: usize,
    /// Body-field initializers in *source* (evaluation) order, each tagged
    /// with its declared position in the variant's own field list -- same
    /// contract as `CheckedStructLiteral::fields`. Always empty for a
    /// body-less variant.
    pub fields: Vec<CheckedStructLiteralField>,
}

/// A whole struct value built in one expression. `fields` is in *source*
/// (evaluation) order -- the order the user wrote the initializers in, which
/// is the order their side effects must run in -- with each entry carrying
/// the field's declared position (`field_index`) in the struct's own field
/// list. Analysis guarantees every declared field appears exactly once and
/// every value already has its field's exact type, so codegen only has to
/// evaluate in list order and emit leaves in `field_index` order.
#[derive(Debug, Clone)]
pub struct CheckedStructLiteral {
    pub fields: Vec<CheckedStructLiteralField>,
}

/// See `CheckedStructLiteral`.
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

/// `base[range]` -- `base`'s resolved type is guaranteed to be `SizedArray`
/// or `Slice` (never anything else) by the time this is constructed, and
/// `start`/`end` (when present) are guaranteed `I32`. `item_type` is
/// `base`'s element type, carried the same way `CheckedProjection::Index`'s
/// is, so codegen never has to re-derive it from `base`'s type. `inclusive`
/// mirrors `omega_hir::HirRange`'s -- when `true` and `end` is present,
/// codegen includes `end` itself in the slice (see
/// `omega_parser::ast::range::RangeExpr`'s doc comment for the range
/// grammar).
#[derive(Debug, Clone)]
pub struct CheckedSlice {
    pub base: CheckedPlace,
    pub item_type: ResolvedType,
    pub start: Option<Box<CheckedExprNode>>,
    pub end: Option<Box<CheckedExprNode>>,
    pub inclusive: bool,
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
    /// `value.tag` on an enum -- reads the tag, which every enum value has
    /// (implicit-tag enums included). `r#type` is the enum's tag type.
    EnumTag {
        r#type: ResolvedType,
    },
    /// A shared header field on an enum value -- present on every variant,
    /// so no static variant knowledge is required. `index` is the field's
    /// position in `ResolvedEnumType::header`; `field` is its name, carried
    /// for diagnostics only (header fields are per-variant constants, so an
    /// assignment through this projection is rejected by name).
    EnumHeader {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    /// A shared *dynamic* field on an enum value -- `EnumHeader`'s sibling:
    /// present on every variant, so no static variant knowledge is
    /// required either, but (unlike `EnumHeader`) ordinary per-instance
    /// storage, not a per-variant constant -- an assignment through this
    /// projection is perfectly ordinary, exactly like `EnumBody`'s.
    /// `index` is the field's position in `ResolvedEnumType::dynamic_fields`.
    EnumDynamicField {
        field: Ident,
        index: usize,
        r#type: ResolvedType,
    },
    /// A body field of an enum value whose variant is statically known --
    /// analysis guarantees the base's resolved type carries exactly
    /// `variant_index` (see `ResolvedType::Enum`), so codegen can compute
    /// the field's offset inside the union region with no runtime check.
    EnumBody {
        variant_index: usize,
        field_index: usize,
        r#type: ResolvedType,
    },
    /// A field of a union value -- deliberately not `FieldAccess`: every
    /// union field lives at offset 0 (they all overlap the same storage), so
    /// codegen never needs (or wants) an index-based offset lookup here, the
    /// way it does for a struct's sequentially laid out fields.
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

/// See `CheckedExpr::Match`'s doc comment.
#[derive(Debug, Clone)]
pub struct CheckedMatch {
    pub arms: Vec<CheckedMatchArm>,
    /// `None` only when analysis already proved every value is covered by
    /// `arms` (`Analyzer::analyze_match`'s exhaustiveness check) -- codegen
    /// (`omega_codegen`'s `emit_match`) traps instead of falling through in
    /// that case, since falling off the end is otherwise provably
    /// unreachable, unlike `CheckedIf`'s "no `else`" case (which defaults to
    /// producing `Void`).
    pub else_branch: Option<CheckedBlock>,
}

/// One arm: every condition must hold, in order, for this arm to run (short-
/// circuiting on the first `false` -- see `emit_match`). A value/enum-variant
/// pattern has exactly one condition; a range pattern has one per bound
/// actually present (so up to two) -- there is no boolean AND/OR operator in
/// this language, so a multi-bound test is nested branching in codegen
/// rather than one merged boolean expression.
#[derive(Debug, Clone)]
pub struct CheckedMatchArm {
    pub conditions: Vec<CheckedExprNode>,
    pub body: CheckedBlock,
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
