use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::{CodeblockExpr, ExpressionNode, MacroInvocationExpr};
use crate::ast::generics::GenericParam;
use crate::ast::identifier::{Ident, Origin};
use crate::ast::self_mode::SelfMode;
use crate::ast::r#type::{FunctionType, Type, Param};
use crate::ast::visibility::Visibility;
use crate::diagnostics::Span;

// Function scope statements
#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    /// `ident : type = value;` -- unlike `Walrus`, the type is written down
    /// explicitly, so lowering can desugar this straight into a plain
    /// `Declaration` + assignment pair itself (see `lower_stmt`), with no
    /// need for semantic analysis to infer anything first.
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ExternDeclaration(ExternDeclarationStmt),
    Expression(ExpressionNode),
    /// `name$(arg, ...);` as a whole statement. Expansion splices its
    /// resulting statements in place; this never reaches HIR lowering.
    MacroInvocation(MacroInvocationExpr),
    Return(ReturnStmt),
    /// No label yet (just `break;`/`continue;`) -- analysis already resolves
    /// these against a stack of enclosing loops keyed by identity rather
    /// than always assuming "the innermost one," specifically so a labeled
    /// `break 'outer;` can be added later by changing only how that
    /// resolution picks an entry, with no parser/HIR/codegen rework (see
    /// `Analyzer`'s `loop_stack`).
    Break,
    Continue,
    Walrus(WalrusStmt),
    While(WhileStmt),
    /// `loop { ... }` -- see `LoopStmt`'s own doc comment for how this
    /// differs from `While` beyond just missing a condition: it's what
    /// makes a function's `never` return type provable at all.
    Loop(LoopStmt),
    /// Boxed since `ForStmt.init` embeds a bare `Statement` -- without the
    /// indirection here, `Statement` would have infinite size.
    For(Box<ForStmt>),
    /// `for <mut>? binding in iterator { ... }` -- see `ForInStmt`'s doc
    /// comment. No infinite-size concern here (`ForInStmt` embeds only
    /// expressions/a codeblock, never a bare `Statement`), but boxed
    /// anyway for consistency with `For`'s own sizing and to keep this
    /// enum's largest variant small.
    ForIn(Box<ForInStmt>),
    /// `defer <statement>;` / `defer { ... }` -- see `DeferStmt`'s doc
    /// comment. Unlike `For`, no extra `Box` is needed at this level:
    /// `DeferStmt` itself already boxes its embedded `Statement`
    /// (`DeferStmt.body: Box<Statement>`), which is what breaks the
    /// recursive-size cycle here.
    Defer(DeferStmt),
}

#[derive(Debug, Clone)]
pub struct StatementNode {
    pub statement: Statement,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    /// The declared name only -- where a "declared twice" diagnostic points.
    pub name_span: Span,
    /// The whole `name: Type`. A struct/union/enum field and a function
    /// parameter are both this node and neither is wrapped in an
    /// `ItemNode`/`StatementNode`, so without these every diagnostic
    /// anchored on one would inherit the enclosing declaration's span --
    /// which meant a duplicate field underlined the entire struct.
    pub span: Span,
    pub origin: Origin,
    pub r#type: Type,
    /// `true` only for a statement-position `mut ident: Type;` -- always
    /// `false` for a struct/enum field or an ordinary function parameter,
    /// since `mut` is never recognized in those positions at all
    /// (`parse_declaration_list` doesn't check for it). `self` is the one
    /// exception: `mut self` (by value) desugars to an immutable `self`
    /// parameter plus an implicit `mut self := self;` shadow -- see
    /// `FunctionDefinitionStmt::self_mode`/`SelfMode` and
    /// `omega_hir::lower::Lowerer::self_param`. See
    /// `omega_analyzer::context::VarBinding::mutable`.
    pub mutable: bool,
    /// `exposed`/`internal`/(default `Hidden`) -- same "meaningless in
    /// most of this shared type's positions" treatment as `mutable`:
    /// genuinely meaningful for a struct/union/enum-dynamic/enum-variant
    /// field or a top-level global declaration (`Item::Declaration`), left
    /// at its default everywhere else (function/spec parameters, local
    /// statement declarations), since none of those positions ever check
    /// for a leading `exposed`/`internal` at all.
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
    /// `exposed`/`internal`/(default `Hidden`) -- an `extern` declaration
    /// is an ordinary top-level item like any other, so it gets the same
    /// treatment.
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct ReturnStmt {
    pub return_value: ExpressionNode,
}

/// `ident := value;` -- "declare and assign", with `ident`'s type inferred
/// from `value`'s resolved type rather than written out explicitly like
/// `DeclarationStmt`. Shared by function-body statements (`Statement::
/// Walrus`) and top-level items (`Item::Walrus`, only legal `comp` -- see
/// `Item::Walrus`'s own doc comment); `visibility` is meaningful only for
/// the latter, left at its default (`Hidden`) for a local statement, same
/// "meaningless in most positions" treatment `DeclarationStmt::mutable`/
/// `visibility` document.
#[derive(Debug, Clone)]
pub struct WalrusStmt {
    pub ident: Ident,
    pub origin: Origin,
    pub value: ExpressionNode,
    /// `true` only for `mut ident := value;`. See
    /// `omega_analyzer::context::VarBinding::mutable`.
    pub mutable: bool,
    /// `true` only for `comp ident := value;` -- `ident` carries no storage
    /// of its own; every reference to it is substituted with its already-
    /// evaluated value at compile time. Never `true` together with
    /// `mutable` in a checked tree (rejected during analysis, not parsing
    /// -- see `AnalysisErrorKind::MutCompBinding`). See
    /// `docs/language/compile-time-evaluation.md`.
    pub comp: bool,
    /// `exposed`/`internal`/(default `Hidden`) -- see this type's own doc
    /// comment for when this is meaningful.
    pub visibility: Visibility,
}

/// `while cond { ... }` -- a plain statement, not an expression: unlike
/// `if`, a loop's body may run zero or many times, so there's no single
/// "the value it produced" to speak of (this language has no `break
/// <value>` either).
#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub condition: ExpressionNode,
    pub body: CodeblockExpr,
}

/// `loop { ... }` -- an unconditional loop, exiting only via `break` (or
/// never, if none is reached). Unlike `WhileStmt`, there is no condition
/// at all: this is the one shape the analyzer can prove always repeats
/// unless a `break` targeting it is found anywhere in its own body (see
/// `Analyzer::stmt_diverges`), which is what lets a function ending in
/// `loop { }` satisfy a `never` return type. Still a plain statement, not
/// an expression, for the same reason `WhileStmt` is (see its own doc
/// comment) -- this language has no `break <value>`.
#[derive(Debug, Clone)]
pub struct LoopStmt {
    pub body: CodeblockExpr,
}

/// `for init; cond; post { ... }` -- classic C-style, three semicolon
/// separated clauses (each independently optional, e.g. `for ;; { ... }` is
/// a valid, deliberately infinite loop) followed by the body. Like `while`,
/// this is a plain statement, never an expression.
///
/// `init` reuses exactly the shapes `Statement` already has for
/// declare-and-assign (`Walrus`, `Declaration`(`WithInit`)) or a plain
/// expression -- but parsed *without* consuming a trailing `;` itself, since
/// the `for` loop's own grammar supplies the `;` separators between clauses.
/// `return`/`extern`/`struct` aren't included: none of them make sense as a
/// loop's init clause.
#[derive(Debug, Clone)]
pub struct ForStmt {
    pub init: Option<Statement>,
    pub condition: Option<ExpressionNode>,
    pub post: Option<ExpressionNode>,
    pub body: CodeblockExpr,
}

/// `for <mut>? binding in iterator { ... }` -- the iteration-protocol loop,
/// distinct from `ForStmt`'s classic C-style three-clause form (both start
/// with `for`; `parser::statement::parse_for` disambiguates by lookahead,
/// the same way it already disambiguates a walrus/declaration/expression
/// init clause). `binding`/`mutable` mirror `WalrusStmt`'s own shape --
/// exactly one plain identifier, no destructuring, matching every other
/// binding form this language has. `iterator` keeps its own natural type;
/// what it must resolve to (something implementing `core::iterator::
/// ToIterator<T>`) is entirely an analysis-time concern -- see
/// `Analyzer::analyze_for_in`.
#[derive(Debug, Clone)]
pub struct ForInStmt {
    pub mutable: bool,
    pub binding: Ident,
    /// An optional element type (`for value : u8 in bytes`) selecting a
    /// particular `ToIterator<T>` implementation when more than one exists.
    pub binding_type: Option<Type>,
    pub iterator: ExpressionNode,
    pub body: CodeblockExpr,
}

/// `defer <statement>;` / `defer { ... }` -- schedules `body` to run when
/// the *enclosing function* exits (see `omega_hir::hir::HirDefer` and
/// `omega_codegen`'s epilogue for how). `body` is a bare `Statement`, not a
/// `StatementNode` -- it has no span of its own; lowering reuses the
/// enclosing `defer` statement's span for it, the same way `ForStmt.init`
/// already does for its own wrapped `Statement`.
#[derive(Debug, Clone)]
pub struct DeferStmt {
    pub body: Box<Statement>,
}

#[derive(Debug, Clone)]
pub struct FunctionDefinitionStmt {
    /// `@inline(...)`/`@mangling(...)`/`@suppress(...)` written directly
    /// above this function -- applies identically whether this is a
    /// top-level function or a struct/enum/union method, since both are
    /// this same node (see `self_mode`). See `omega_analyzer::annotations`.
    pub annotations: Vec<AnnotationNode>,
    /// `exposed`/`internal`/(default `Hidden`) -- applies identically
    /// whether this is a top-level function or a struct/enum/union method,
    /// same dual-purpose treatment as `self_mode`.
    pub visibility: Visibility,
    pub ident: Ident,
    /// The function name only.
    pub name_span: Span,
    /// From the name through the declared return type, excluding the body.
    pub signature_span: Span,
    /// The declared return type only.
    pub return_type_span: Span,
    /// `<T, U, ...>` immediately after `ident` -- empty for an
    /// ordinary, non-generic function. Unlike a struct's, these are never
    /// referenced with explicit arguments at a call site: they're deduced
    /// from the call's own argument types (see `Analyzer::resolve_generic_call`).
    /// A bound generic (`T: Animal`) additionally requires the deduced
    /// argument type to nominally implement that spec.
    pub generics: Vec<GenericParam>,
    /// `None` for an ordinary, non-member function; `Some` for a
    /// struct/enum/union method, carrying exactly how `self` was written
    /// (`self`/`mut self`/`*self`/`*mut self`) -- determines the
    /// synthesized `self` parameter's type (see
    /// `omega_hir::lower::Lowerer::lower_function_def`).
    pub self_mode: Option<SelfMode>,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub codeblock: CodeblockExpr,
}

impl FunctionDefinitionStmt {
    /// This definition's signature as a `Type::Function`. Since both sides
    /// carry the same `Param` node now, this no longer rebuilds a parallel
    /// `(Ident, Type)` list field by field.
    pub fn function_type(&self) -> FunctionType {
        FunctionType {
            params: self.params.clone(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: false,
            self_mode: self.self_mode,
        }
    }
}
