use crate::ids::HirId;
// Re-exported: `HirBinaryOp.op`'s type needs to be nameable by downstream
// crates (codegen matches on its variants) without them depending on
// omega-parser directly, the same way they never need to spell `Ident`/
// `Type` because they only ever go through field access, never pattern
// match on those.
pub use omega_parser::prelude::{BinaryOp, ImportRoot};
use omega_parser::prelude::{ByteStringExpr, ExprPath, FunctionType, Ident, NumberExpr, Path, Span, StringExpr, Type};

/// A lowered `@name(args)` annotation -- mechanical clone of `omega_parser`'s
/// `AnnotationNode`/`AnnotationArg` (see their doc comments), unvalidated:
/// which names exist, which item kinds they're allowed on, and what their
/// arguments mean is `omega_analyzer::annotations`'s job, not lowering's.
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

/// Mirror of `omega_parser`'s `AnnotationValue` -- see its doc comment.
#[derive(Debug, Clone)]
pub enum HirAnnotationValue {
    IntLiteral(String),
    Sizeof(Type),
}

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
    Enum(HirEnumDef),
    Union(HirUnionDef),
    Spec(HirSpecDef),
    Import(HirImport),
}

/// One `<...>` generic parameter -- a name, plus an optional single spec
/// bound, kept as a raw, unresolved `Type` (resolved per-instantiation, the
/// same way every other type reference in HIR is). See
/// `omega_parser::ast::generics::GenericParam`'s doc comment for why only
/// one bound is ever carried.
#[derive(Debug, Clone)]
pub struct HirGenericParam {
    pub ident: Ident,
    pub bound: Option<Type>,
}

/// `import a::b::c;` -- carried raw and unresolved, same philosophy as every
/// other HIR node (lowering never does symbol resolution). Whether `path`
/// names a whole module or an item inside one is decided later by
/// `omega_analyzer::resolver::ModuleResolver`. `root` says what `path` is
/// anchored to (see `ImportRoot`'s own doc comment) -- turning the two into
/// an actual absolute module path is `omega_driver::Driver::
/// import_absolute_path`'s job.
#[derive(Debug, Clone)]
pub struct HirImport {
    pub id: HirId,
    pub span: Span,
    pub root: ImportRoot,
    pub path: Path,
}

#[derive(Debug, Clone)]
pub struct HirDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: Type,
    /// See `omega_parser::ast::statement::declaration::DeclarationStmt::mutable`.
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct HirExternDeclaration {
    pub id: HirId,
    pub span: Span,
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
    pub span: Span,
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
    pub span: Span,
    /// `@inline(...)`/`@mangling(...)`/`@suppress(...)` -- see
    /// `omega_analyzer::annotations::resolve`.
    pub annotations: Vec<HirAnnotation>,
    pub name: Ident,
    /// `<T, U, ...>` -- empty for an ordinary, non-generic function. See
    /// `omega_parser::ast::statement::function_definition::
    /// FunctionDefinitionStmt::generics`'s doc comment.
    pub generics: Vec<HirGenericParam>,
    pub is_member_function: bool,
    /// For member functions, the synthetic `self: *StructName` parameter is
    /// already inserted here by lowering -- downstream consumers never need
    /// to special-case it.
    pub params: Vec<HirParam>,
    pub return_type: Type,
    pub body: HirBlock,
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
    pub span: Span,
    /// `@packing(...)`/`@suppress(...)` -- see
    /// `omega_analyzer::annotations::resolve`.
    pub annotations: Vec<HirAnnotation>,
    pub name: Ident,
    /// `<T, U, ...>` -- empty for an ordinary, non-generic struct. See
    /// `omega_parser::ast::statement::r#struct::StructStmt::generics`'s
    /// doc comment.
    pub generics: Vec<HirGenericParam>,
    /// The specs this struct implements -- see `HirSpecDef`'s doc comment
    /// and `Analyzer::signature_of_struct`'s implements-clause resolution.
    pub implements: Vec<Type>,
    pub fields: Vec<HirParam>,
    pub functions: Vec<HirFunctionDef>,
}

/// A C/Rust-style union -- see `omega_parser::ast::statement::union::
/// UnionStmt`'s doc comment; structurally identical to `HirStructDef`
/// (fields overlap in storage instead of being laid out sequentially is
/// entirely an analyzer/codegen concern, not a lowering one).
#[derive(Debug, Clone)]
pub struct HirUnionDef {
    pub id: HirId,
    pub span: Span,
    /// `@suppress(...)` -- `@packing` isn't recognized on a union yet. See
    /// `omega_analyzer::annotations::resolve`.
    pub annotations: Vec<HirAnnotation>,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    /// See `HirStructDef::implements`'s doc comment.
    pub implements: Vec<Type>,
    pub fields: Vec<HirParam>,
    pub functions: Vec<HirFunctionDef>,
}

/// An omega-style enum -- see `omega_parser::ast::statement::r#enum::
/// EnumStmt`'s doc comment for the full language shape. Carried raw like
/// every other HIR node: whether the header's first entry is a valid
/// explicit tag, whether each variant's `args` are constants of the right
/// types, and whether tags are unique are all analysis's questions.
#[derive(Debug, Clone)]
pub struct HirEnumDef {
    pub id: HirId,
    pub span: Span,
    /// `@packing(...)`/`@suppress(...)` -- see
    /// `omega_analyzer::annotations::resolve`.
    pub annotations: Vec<HirAnnotation>,
    pub name: Ident,
    /// `<T, U, ...>` -- empty for an ordinary, non-generic enum.
    pub generics: Vec<HirGenericParam>,
    /// See `HirStructDef::implements`'s doc comment.
    pub implements: Vec<Type>,
    /// The raw header entries, in source order -- a first entry named `tag`
    /// is the explicit tag; the rest are the shared header fields.
    pub header: Vec<HirParam>,
    /// The optional shared-dynamic-fields section -- present on every
    /// variant like `header`, but (unlike `header`) runtime-valued, not a
    /// per-variant constant; empty when the enum declares none.
    pub dynamic_fields: Vec<HirParam>,
    pub variants: Vec<HirEnumVariant>,
    /// Same shape as `HirStructDef::functions` -- a member function's
    /// synthetic `self: *EnumName` parameter is already inserted by
    /// lowering.
    pub functions: Vec<HirFunctionDef>,
}

/// One enum variant -- self-identifying (`id`/`span`, the span covering the
/// variant's name) like every declaration-shaped HIR node, since identity
/// problems (duplicate name/tag, wrong `args` count) anchor here.
#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    /// The variant's header values (explicit tag first, when the enum
    /// declares one) -- analysis requires these to be constants.
    pub args: Vec<HirExprNode>,
    /// The variant's own body fields -- empty for a body-less variant.
    pub fields: Vec<HirParam>,
}

/// A `spec` -- see `omega_parser::ast::statement::spec::SpecStmt`'s doc
/// comment for the full language shape (declaration vs. alias forms; both
/// lower to this one shape, an alias just has `functions: vec![]`).
/// `dependencies` and each `HirSpecFunction`'s `params`/`return_type` are
/// kept raw/unresolved, same philosophy as everywhere else in HIR -- a
/// `Self`-referencing type can't be resolved until a concrete implementor
/// is known (see `omega_analyzer::resolved_type::ResolvedSpecType`).
#[derive(Debug, Clone)]
pub struct HirSpecDef {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub generics: Vec<HirGenericParam>,
    pub dependencies: Vec<Type>,
    pub functions: Vec<HirSpecFunction>,
}

/// One function member of a spec. `body: None` for a required function --
/// every implementor must provide a matching method, own or default;
/// `body: Some` for a default, used as-is unless a concrete implementor
/// overrides it with its own same-named, same-signature method.
#[derive(Debug, Clone)]
pub struct HirSpecFunction {
    pub id: HirId,
    pub span: Span,
    pub name: Ident,
    pub is_member_function: bool,
    /// For a member function, the synthetic `self: *Self`/`*mut Self`
    /// parameter is already inserted here by lowering, exactly like an
    /// ordinary method's -- see `lower_function_def`'s spec-aware case.
    pub params: Vec<HirParam>,
    pub return_type: Type,
    pub body: Option<HirBlock>,
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Declaration(HirDeclaration),
    /// `ident : Type = value;` -- kept as one node (rather than a
    /// `Declaration` followed by a separate assignment expression) so the
    /// initializing write can be checked as exactly that -- initialization,
    /// never a `mut`-requiring reassignment -- the same way
    /// `WalrusDeclaration`'s own initializer already is. See
    /// `Analyzer::analyze_declaration_with_init`.
    DeclarationWithInit(HirDeclaration, HirExprNode),
    ExternDeclaration(HirExternDeclaration),
    Expression(HirExprNode),
    Return(HirExprNode),
    WalrusDeclaration(HirWalrusDeclaration),
    While(HirWhile),
    For(HirFor),
    Break(HirBreak),
    Continue(HirContinue),
    Defer(HirDefer),
}

/// `defer <statement>;` / `defer { ... }` -- schedules `body` to run when
/// the *enclosing function* exits (every `return`, and the implicit
/// fallthrough at the end of a void function), in FILO order relative to
/// other defers in the same function. Structural, not value-producing, like
/// `HirBreak`/`HirContinue` -- `body` is what's deferred, `id`/`span` self-
/// identify the `defer` statement itself (its own position is what a
/// runtime "was this defer reached" flag gets set at; see
/// `omega_codegen`'s epilogue).
#[derive(Debug, Clone)]
pub struct HirDefer {
    pub id: HirId,
    pub span: Span,
    pub body: HirBlock,
}

/// `break;` -- no label yet; see `Statement::Break`'s doc comment for why
/// that's fine to add later without disturbing this shape (just a new
/// `label: Option<Ident>` field here and a corresponding one on
/// `CheckedBreak`).
#[derive(Debug, Clone)]
pub struct HirBreak {
    pub id: HirId,
    pub span: Span,
}

/// `continue;` -- see `HirBreak`.
#[derive(Debug, Clone)]
pub struct HirContinue {
    pub id: HirId,
    pub span: Span,
}

/// A brace-delimited sequence of statements plus an optional final
/// expression with no trailing `;` -- the block's own value (see
/// `CodeblockExpr`'s doc comment in the parser). Shared by bare
/// `{ ... }` expressions, `if`/`else` branches, `while`/`for` bodies, and a
/// function's own body -- all of them have identical "statements, then
/// maybe a tail" shape and identical analysis (see `Analyzer::analyze_block`).
#[derive(Debug, Clone)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
    pub tail: Option<Box<HirExprNode>>,
}

/// `while cond { body }` -- a statement, not an expression: see
/// `omega_parser::ast::statement::while_stmt::WhileStmt`'s doc comment.
/// Self-identifying (`id`/`span`), like every other statement-shaped HIR
/// node, since analysis needs somewhere to anchor a `NonBoolCondition` error
/// that isn't attached to `condition`/`body` specifically.
#[derive(Debug, Clone)]
pub struct HirWhile {
    pub id: HirId,
    pub span: Span,
    pub condition: HirExprNode,
    pub body: HirBlock,
}

/// `for init; cond; post { body }` -- `init` is a list rather than a single
/// optional statement because lowering a `DeclarationWithInit` init clause
/// (`for i : i32 = 0; ...`) produces *two* `HirStmt`s (see `lower_statement`);
/// empty means the clause was omitted (`for ;cond; ...`). `condition` is
/// required to actually be present by analysis (`AnalysisErrorKind::
/// ForLoopMissingCondition`) even though the grammar itself allows omitting
/// it -- see `HirFor`'s counterpart `CheckedFor` in `omega-analyzer` for why.
#[derive(Debug, Clone)]
pub struct HirFor {
    pub id: HirId,
    pub span: Span,
    pub init: Vec<HirStmt>,
    pub condition: Option<HirExprNode>,
    pub post: Option<HirExprNode>,
    pub body: HirBlock,
}

/// `ident := value;` -- unlike `HirDeclaration`, there's no `Type` to carry
/// here (there's nothing written down to carry): the declared variable's
/// type is inferred from `value`'s resolved type, which only analysis can
/// determine. Lowering can't desugar this into a plain `HirDeclaration` +
/// assignment pair itself for exactly that reason; analysis does once it
/// knows `value`'s type (see `analyze_stmt` in `omega-analyzer`).
#[derive(Debug, Clone)]
pub struct HirWalrusDeclaration {
    pub id: HirId,
    pub span: Span,
    pub ident: Ident,
    pub value: HirExprNode,
    /// See `omega_parser::ast::statement::walrus::WalrusStmt::mutable`.
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct HirExprNode {
    pub id: HirId,
    pub span: Span,
    pub expr: HirExpr,
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Place(HirPlace),
    Number(NumberExpr),
    String(StringExpr),
    /// `b"..."` -- see `Expression::ByteString`'s doc comment.
    ByteString(ByteStringExpr),
    Bool(bool),
    Char(char),
    Codeblock(HirBlock),
    /// `if cond { ... } else if cond { ... } else { ... }` -- see
    /// `omega_parser::ast::expression::if_expr::IfExpr`'s doc comment;
    /// `branches` is always non-empty (the leading `if` is `branches[0]`).
    If(HirIf),
    FunctionCall(HirFunctionCall),
    Assignment(HirAssignment),
    /// `target op= value` -- see `HirCompoundAssign`'s doc comment.
    CompoundAssign(HirCompoundAssign),
    AddressOf(HirAddressOf),
    Negate(Box<HirExprNode>),
    /// `~base` -- see `Expression::BitNot`'s doc comment.
    BitNot(Box<HirExprNode>),
    /// `++base`/`--base` -- `base` isn't guaranteed to be a place at this
    /// level, same treatment as `AddressOf`/`Assignment`'s target; see
    /// `Analyzer::analyze_incr_decr`, which both validates that and performs
    /// the actual "add or subtract one" desugaring.
    Increment(Box<HirExprNode>),
    Decrement(Box<HirExprNode>),
    BinaryOp(HirBinaryOp),
    /// `[e1, e2, ...]` -- a fixed-size array value. Its size is implicitly
    /// `elements.len()`; there's nothing else to resolve structurally here,
    /// so lowering just lowers each element in place -- the common resolved
    /// element type, and therefore the whole literal's `ResolvedType`, is
    /// only known once semantic analysis has typed every element.
    ArrayLiteral(Vec<HirExprNode>),
    /// `Name { field = value; ... }` -- a whole struct value built in one
    /// expression. Carried raw (`path` unresolved, fields by name), same
    /// philosophy as every other HIR node: whether `path` names a struct,
    /// whether every field exists and is covered exactly once, and each
    /// value's type are all analysis's questions.
    StructLiteral(HirStructLiteral),
    Slice(HirSlice),
    /// See `HirMatch`'s doc comment.
    Match(HirMatch),
    /// `<Type>base` -- `target` is carried raw and unresolved, same
    /// philosophy as every other HIR node: whether it's actually castable
    /// (and, if so, exactly which conversion it needs) is analysis's
    /// question (see `omega_analyzer::resolved_type::ResolvedType::cast_class`).
    Cast(HirCast),
    /// `sizeof<Type>` -- `Type` carried raw/unresolved, same philosophy as
    /// `Cast`'s `target`; no `base` at all (see `SizeofExpr`'s doc comment).
    Sizeof(Type),
}

/// See `HirExpr::Cast`.
#[derive(Debug, Clone)]
pub struct HirCast {
    pub target: Type,
    pub base: Box<HirExprNode>,
}

/// See `HirExpr::StructLiteral`.
#[derive(Debug, Clone)]
pub struct HirStructLiteral {
    pub path: ExprPath,
    pub fields: Vec<HirStructLiteralField>,
}

/// One `name: value;` initializer -- `name_span` points at the field name
/// itself, for "no such field"/"field set twice" diagnostics.
#[derive(Debug, Clone)]
pub struct HirStructLiteralField {
    pub name: Ident,
    pub name_span: Span,
    pub value: HirExprNode,
}

/// See `HirExpr::If`'s doc comment.
#[derive(Debug, Clone)]
pub struct HirIf {
    pub branches: Vec<(HirExprNode, HirBlock)>,
    pub else_branch: Option<HirBlock>,
}

/// `base[range]` -- produces a new slice (fat pointer) over a sub-range of
/// `base`, unlike `HirProjection::Index` which produces a single element.
/// `base` is a place (same as `HirAddressOf.base`'s treatment, minus the
/// "must be a place" enforcement, which analysis still has to do here too)
/// rather than a plain expression, since slicing needs to know exactly what's
/// being sliced -- a `SizedArray`'s inline storage vs. an existing `Slice`'s
/// data pointer+length -- the same distinction indexing has to make.
#[derive(Debug, Clone)]
pub struct HirSlice {
    pub base: HirPlace,
    pub range: HirRange,
}

/// See `omega_parser::ast::range::RangeExpr`'s doc comment for the range
/// grammar itself -- shared, unchanged, between slicing (`HirSlice`) and
/// match range-patterns (`HirPattern::Range`).
#[derive(Debug, Clone)]
pub struct HirRange {
    pub start: Option<Box<HirExprNode>>,
    pub end: Option<Box<HirExprNode>>,
    pub inclusive: bool,
    pub span: Span,
}

/// `match scrutinee { pattern => body, ... } else { ... }` -- see
/// `omega_parser::ast::expression::match_expr::MatchExpr`'s doc comment.
/// Carried raw, same philosophy as every other HIR node: whether each
/// pattern is well-formed for the scrutinee's type, whether the arms are
/// exhaustive, and any place-narrowing inside a matched arm are all
/// analysis's questions.
#[derive(Debug, Clone)]
pub struct HirMatch {
    pub scrutinee: Box<HirExprNode>,
    pub arms: Vec<HirMatchArm>,
    pub else_branch: Option<HirBlock>,
}

/// See `HirMatch`'s doc comment.
#[derive(Debug, Clone)]
pub struct HirMatchArm {
    pub pattern: HirPattern,
    pub body: HirExprNode,
    pub span: Span,
}

/// One arm's pattern -- see `omega_parser::ast::expression::match_expr::Pattern`'s
/// doc comment; carried just as raw as everywhere else, telling a literal
/// apart from an `Enum::Variant` path is analysis's job.
#[derive(Debug, Clone)]
pub enum HirPattern {
    Value(HirExprNode),
    Range(HirRange),
}

impl HirPattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Value(v) => v.span,
            Self::Range(r) => r.span,
        }
    }
}

/// The parser has no notion of "places"/lvalues -- it only knows `Ident`,
/// `FieldAccess`, `Index`, and `Deref` as plain expression-forming
/// constructs (see `omega_parser::ast::expression`). Lowering is what
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
    /// A (possibly module-qualified, possibly generic-argumented) path -- a
    /// bare identifier is just the degenerate one-segment case, same as
    /// everywhere else `Path`/`ExprPath` is used.
    Path(ExprPath),
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

/// `left op right` -- `BinaryOp` is a plain data tag with no
/// parser-specific structure, so it's reused unchanged from
/// `omega_parser::prelude` rather than re-wrapped at this layer, the same
/// way `Ident`/`Type` already are.
#[derive(Debug, Clone)]
pub struct HirBinaryOp {
    pub op: BinaryOp,
    pub left: Box<HirExprNode>,
    pub right: Box<HirExprNode>,
}

/// `&base` -- unlike `Deref`, this never denotes a place itself (it produces
/// a pointer *value*). `base` is lowered as an ordinary expression, not
/// structurally guaranteed to be a place at this level -- same treatment as
/// `HirAssignment.target`, validated during analysis.
#[derive(Debug, Clone)]
pub struct HirAddressOf {
    pub base: Box<HirExprNode>,
    /// See `omega_parser::ast::expression::address_of::AddressOfExpr::mutable`.
    pub mutable: bool,
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

/// `target op= value` -- same "not guaranteed to be a place yet" treatment
/// as `HirAssignment.target`; see `Analyzer::analyze_compound_assign`,
/// which both validates that and performs the actual `target = target op
/// value` desugaring.
#[derive(Debug, Clone)]
pub struct HirCompoundAssign {
    pub target: Box<HirExprNode>,
    pub op: BinaryOp,
    pub value: Box<HirExprNode>,
}
