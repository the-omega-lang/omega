use crate::ast::identifier::{ExprPath, Ident, Origin};
use crate::ast::range::RangeExpr;
use crate::ast::statement::StatementNode;
use crate::ast::r#type::Type;
use crate::diagnostics::Span;
use crate::lexer::Token;

/// The parser only knows syntax, not semantics: `FieldAccess`/`Index`/`Deref`/
/// `BinaryOp` are just expression-forming operators here, the same as
/// `FunctionCall`. There is no "place"/lvalue concept at this layer --
/// deciding which expression shapes denote an addressable location is HIR
/// lowering's job, and no type-checking happens here either.
#[derive(Debug, Clone)]
pub enum Expression {
    /// A (possibly module-qualified) path -- `foo`, or `mymodule::thing::foo`,
    /// or one with explicit generic arguments on a segment
    /// (`Optional<u32>::Some`). A bare, unqualified name is just the
    /// degenerate one-segment case; see `Path`/`ExprPath`'s own doc comments.
    Path(ExprPath),
    FieldAccess(Box<FieldAccessExpr>),
    Index(Box<IndexExpr>),
    Deref(Box<DerefExpr>),
    AddressOf(Box<AddressOfExpr>),
    /// `reveal base` -- see `RevealExpr`'s doc comment.
    Reveal(Box<RevealExpr>),
    /// `comp base` -- see `CompExpr`'s doc comment.
    Comp(Box<CompExpr>),
    Negate(Box<NegateExpr>),
    /// `~base` -- see `BitNotExpr`'s doc comment.
    BitNot(Box<BitNotExpr>),
    /// `!base` -- see `NotExpr`'s doc comment.
    Not(Box<NotExpr>),
    /// `a && b` / `a || b` -- see `LogicalExpr`'s doc comment. Kept apart
    /// from `BinaryOp` because these are the only two operators whose right
    /// operand may not be evaluated at all, which makes them control flow,
    /// not arithmetic.
    Logical(Box<LogicalExpr>),
    /// `<Type>base` -- see `CastExpr`'s doc comment.
    Cast(Box<CastExpr>),
    /// `sizeof<Type>` -- see `SizeofExpr`'s doc comment.
    Sizeof(Box<SizeofExpr>),
    Increment(Box<IncrementExpr>),
    Decrement(Box<DecrementExpr>),
    BinaryOp(Box<BinaryOpExpr>),
    Number(NumberExpr),
    String(StringExpr),
    /// `b"..."` -- a raw run of bytes, not a null-terminated C string: its
    /// type is `*[]u8` (`ResolvedType::Slice`, a data pointer + length),
    /// never `*u8` the way `String` is. See `Context::resolve_pointer_type`'s
    /// `InferredArray` case for why `*[]u8` already means exactly that.
    ByteString(ByteStringExpr),
    Bool(BoolExpr),
    Char(CharExpr),
    Codeblock(CodeblockExpr),
    If(Box<IfExpr>),
    Match(Box<MatchExpr>),
    FunctionCall(FunctionCallExpr),
    Assignment(Box<AssignmentExpr>),
    /// `target op= value` -- see `CompoundAssignExpr`'s doc comment.
    CompoundAssign(Box<CompoundAssignExpr>),
    ArrayLiteral(ArrayLiteralExpr),
    /// `Name { field = value; ... }` -- see `StructLiteralExpr`'s doc comment.
    StructLiteral(StructLiteralExpr),
    Slice(Box<SliceExpr>),
    /// `name$(arg, ...)` -- expanded away entirely by
    /// `omega_parser::macros::expand` before HIR lowering ever runs; see
    /// `MacroInvocationExpr`'s doc comment.
    MacroInvocation(MacroInvocationExpr),
    /// `a..<b` / `a..=b` / `a..` -- a standalone range, legal *only* as a
    /// range-driven `for` loop's own direct iterator source (`for i in
    /// 10..<20 { ... }`); rejected everywhere else at analysis time. Never
    /// produced by the general expression grammar -- only by the
    /// dedicated `for`-in-iterator parse path (`crate::parser::expression::
    /// parse_range_or_expression`), so this variant is structurally
    /// unreachable anywhere but there. See `crate::ast::range::RangeExpr`'s
    /// doc comment for why this doesn't back a real, general-purpose Range
    /// value type.
    Range(Box<RangeExpr>),
}

#[derive(Debug, Clone)]
pub struct ExpressionNode {
    pub expression: Expression,
    pub span: Span,
}

/// `base.field` -- a plain expression-forming operator. The parser has no
/// notion of "places"/lvalues; it just knows this syntax exists. Whether a
/// given `FieldAccessExpr` chain denotes an addressable location is decided
/// later, during HIR lowering.
#[derive(Debug, Clone)]
pub struct FieldAccessExpr {
    pub base: ExpressionNode,
    pub field: Ident,
}

/// `base[index]` -- a plain expression-forming operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser doesn't know or care
/// whether this denotes an addressable location, only HIR lowering does.
#[derive(Debug, Clone)]
pub struct IndexExpr {
    pub base: ExpressionNode,
    pub index: ExpressionNode,
}

/// `*base` -- a plain expression-forming prefix operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser doesn't know or care
/// that this denotes an addressable location, only HIR lowering does (this
/// one folds into a place the same way `FieldAccess`/`Index` do).
#[derive(Debug, Clone)]
pub struct DerefExpr {
    pub base: ExpressionNode,
}

/// `&base` (`mutable: false`) or `&mut base` (`mutable: true`) -- a plain
/// expression-forming prefix operator. Unlike `Deref`, this never denotes a
/// place itself (it produces a pointer *value*); the parser still doesn't
/// validate that `base` is addressable (or, for `&mut`, mutable), that's
/// HIR lowering/analysis's job, same as an assignment's target.
#[derive(Debug, Clone)]
pub struct AddressOfExpr {
    pub base: ExpressionNode,
    pub mutable: bool,
}

/// `reveal base` -- a visibility-bypass prefix, parsed at the same
/// `parse_unary` precedence tier as `Deref`/`AddressOf` (see
/// `parser::expression::parse_unary`). Unlike those, `base` isn't
/// restricted to place-shaped expressions: `reveal Struct { field = v }` and
/// `reveal foo()` are both legal, so this stays a generic wrapper rather
/// than folding into `HirPlace` -- see `omega_hir::hir::HirExpr::Reveal`'s
/// doc comment for how analysis handles that.
#[derive(Debug, Clone)]
pub struct RevealExpr {
    pub base: ExpressionNode,
}

/// `comp base` -- evaluate `base` at compile time. Parsed at the same
/// `parse_unary` precedence tier as `reveal`/`Deref`/`AddressOf` (see
/// `parser::expression::parse_unary`), and like `reveal`, `base` isn't
/// restricted to place-shaped expressions -- `comp add(10, 20)` and `comp
/// MyThing { field = 1; }` are both legal. See `omega_hir::hir::HirExpr::Comp`'s
/// doc comment for how analysis handles this (an interpreter, not a second
/// type-checker -- `base` is analyzed completely ordinarily first).
#[derive(Debug, Clone)]
pub struct CompExpr {
    pub base: ExpressionNode,
}

/// `-base` -- a plain expression-forming prefix operator, same rationale as
/// [`super::deref::DerefExpr`]. Added alongside binary subtraction: without
/// it there would be no way to write a negative value or negate a variable
/// (`NumberExpr`'s grammar has no sign of its own).
#[derive(Debug, Clone)]
pub struct NegateExpr {
    pub base: ExpressionNode,
}

/// `~base` -- a plain expression-forming prefix operator, same rationale as
/// [`super::negate::NegateExpr`]: unary bitwise-not, integer-only (rejected
/// for `Bool`/`Char`/`Float` during analysis, same as `-`'s own operand
/// restriction).
#[derive(Debug, Clone)]
pub struct BitNotExpr {
    pub base: ExpressionNode,
}

/// `!base` -- logical negation of a `bool`, and the counterpart of `&&`/`||`
/// below. Deliberately a different operator from `~` (`BitNotExpr`): `~`
/// flips a bit pattern and is rejected on `bool` outright, because a
/// bitwise-NOT of `bool`'s `0`/`1` does not stay within `{0, 1}` the way
/// `& | ^` do. `!` is defined *only* on `bool`.
///
/// Analysis desugars this to `base ^ true` once it knows `base` is a `bool`
/// (see `Analyzer::analyze_not`), so nothing downstream -- `CheckedExpr`,
/// MIR, either codegen backend -- needs a representation of its own.
#[derive(Debug, Clone)]
pub struct NotExpr {
    pub base: ExpressionNode,
}

/// Which short-circuiting connective a `LogicalExpr` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp {
    /// `&&` -- `right` is evaluated only if `left` is `true`.
    And,
    /// `||` -- `right` is evaluated only if `left` is `false`.
    Or,
}

/// `left && right` / `left || right`.
///
/// These are *not* `BinaryOp` variants, and the distinction is the whole
/// point: every `BinaryOp` evaluates both operands, whereas these two may
/// not evaluate `right` at all. Omega's `&`/`|` remain available on `bool`
/// and keep evaluating both sides -- so the choice between `a & b` and
/// `a && b` is exactly the choice of whether `b`'s side effects happen, and
/// is visible in the spelling.
///
/// Analysis desugars these into the `if`-expression forms the language has
/// always used by hand (`a && b` is `if a { b } else { false }`), so the
/// short-circuit is real control flow all the way down rather than a
/// special case any backend has to know about.
#[derive(Debug, Clone)]
pub struct LogicalExpr {
    pub op: LogicalOp,
    pub left: ExpressionNode,
    pub right: ExpressionNode,
}

/// `<Type>base` -- a plain expression-forming prefix operator, same
/// left-to-right shape as `NegateExpr`/`DerefExpr`/`AddressOfExpr`. Scoped
/// to numeric conversions (with real width/signedness-aware codegen) and
/// pointer/integer reinterpretation -- the parser doesn't restrict `target`
/// at all (it's the ordinary type grammar), but analysis rejects anything
/// that isn't castable (see `ResolvedType::cast_class`).
#[derive(Debug, Clone)]
pub struct CastExpr {
    pub target: Type,
    pub base: ExpressionNode,
}

/// `sizeof<Type>` -- a compile-time size query, evaluating to the target's
/// `usize`. Unlike `CastExpr`, this has no `base` expression at all: it's a
/// pure function of a type, not an operator applied to a value. Parsed the
/// same way any other generic-looking construct in this grammar is (see
/// `parser::expression::parse_sizeof`); `sizeof` itself is a contextual
/// keyword (like `self`/`mut`), recognized only when immediately followed
/// by `<` -- an ordinary variable named `sizeof` used any other way still
/// parses as a plain identifier.
#[derive(Debug, Clone)]
pub struct SizeofExpr {
    pub r#type: Type,
}

/// `++base` -- sugar for "add one and assign back," but not represented that
/// way syntactically: `base` isn't guaranteed to be a place at this level
/// (same rationale as `AddressOfExpr`/`NegateExpr`), so analysis is what
/// validates it and performs the actual desugaring once it knows `base`'s
/// resolved type (see `Analyzer::analyze_incr_decr` -- the "+1"/"-1" it
/// builds has to match `base`'s exact numeric type, which isn't known here).
#[derive(Debug, Clone)]
pub struct IncrementExpr {
    pub base: ExpressionNode,
}

/// `--base` -- see `IncrementExpr`.
#[derive(Debug, Clone)]
pub struct DecrementExpr {
    pub base: ExpressionNode,
}

/// A plain data tag, no parser-specific structure -- reused unchanged
/// through HIR, analysis, and codegen the same way `Ident`/`Type` already
/// are, rather than re-wrapped at each layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    /// `== != < <= > >=` -- unlike the arithmetic ops above, these always
    /// produce `bool` regardless of the (still-matching) operand type; see
    /// `Analyzer`'s `HirExpr::BinaryOp` arm.
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `& | ^ << >>` -- integer-only (rejects `Float`, same spirit as
    /// `Rem`'s `FloatRemainder`); see `Analyzer::analyze_binary_op`.
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }

    /// The operator as the user wrote it -- for diagnostics ("cannot apply
    /// `%` to ..."), where the variant name (`Rem`) would just be noise.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::Shl => "<<",
            Self::Shr => ">>",
        }
    }
}

/// `left op right` -- a plain expression-forming operator, same rationale as
/// [`super::field_access::FieldAccessExpr`]: the parser only knows this is
/// syntax, not whether/how it type-checks.
#[derive(Debug, Clone)]
pub struct BinaryOpExpr {
    pub left: ExpressionNode,
    pub op: BinaryOp,
    pub right: ExpressionNode,
}

/// Which radix a number literal's integer (and, for `Decimal`, fractional)
/// digits were written in. Kept alongside the digit text rather than eagerly
/// computed into a value here -- the same reason `explicit_type` is kept as
/// `Ident` text -- since only semantic analysis knows which concrete
/// resolved type the literal will end up as, and therefore how to range-check
/// it (`0xFF` might be a `u8`, an `i32`, or anything else numeric).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberBase {
    Decimal,
    Hex,
    Octal,
    Binary,
}

impl NumberBase {
    pub fn radix(self) -> u32 {
        match self {
            Self::Decimal => 10,
            Self::Hex => 16,
            Self::Octal => 8,
            Self::Binary => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NumberExpr {
    pub base: NumberBase,
    pub integer_part: String,
    /// Only ever `Some` for `NumberBase::Decimal` -- the grammar has no
    /// hex/octal/binary float notation (e.g. no `0x1.8p0`), so a fraction is
    /// only ever produced alongside a decimal integer part.
    pub fractional_part: Option<String>,
    pub explicit_type: Option<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StringExpr(pub String);

/// `b"..."` -- decoded content (escapes already resolved), same shape as
/// `StringExpr`; see `Expression::ByteString`'s doc comment for how the two
/// differ downstream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ByteStringExpr(pub String);

/// `true`/`false` -- a bare keyword literal, tried before the general
/// `Path`/identifier case in expression-primary position so the keywords
/// aren't instead parsed as (undefined) variable references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoolExpr(pub bool);

/// `'c'` -- a single Unicode scalar value, single-quote delimited. Shares
/// its escape grammar with `StringExpr`; unlike a string, exactly one
/// character or escape is allowed between the quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharExpr(pub char);

/// `{ stmt; stmt; ... tail }` -- `tail` is the optional final expression with
/// no trailing `;`, whose value is the block's own value.
#[derive(Debug, Clone)]
pub struct CodeblockExpr {
    pub statements: Vec<StatementNode>,
    pub tail: Option<Box<ExpressionNode>>,
    /// The whole block including its braces. A block is a place a
    /// diagnostic can be *about* -- "this block's branches disagree",
    /// "control reaches the end of this block" -- and without a span of its
    /// own it could only borrow the enclosing item's. See
    /// `omega_hir::HirBlock::span`.
    pub span: Span,
}

/// `if cond { ... } else if cond { ... } else { ... }` -- a genuine
/// expression (unlike `while`/`for`), whose value is whichever branch's
/// block ran (see `CodeblockExpr`'s tail expression). `branches` holds every
/// `if`/`else if` condition-block pair in source order (the first entry is
/// always the leading `if`); `else_branch` is the trailing `else`, if any.
/// Analysis is what enforces that every branch (and the `else`, if present)
/// resolves to the same type -- the parser only knows the shape.
#[derive(Debug, Clone)]
pub struct IfExpr {
    pub branches: Vec<(ExpressionNode, CodeblockExpr)>,
    pub else_branch: Option<CodeblockExpr>,
}

/// `match scrutinee { pattern => body, ... } else { ... }` -- an exhaustive
/// switch, and (for an enum scrutinee) the proof mechanism that narrows a
/// matched place to a specific variant subtype inside the arm that proved
/// it (see `Pattern`'s doc comment). Deliberately shaped like `IfExpr`: a
/// genuine expression whose value is whichever arm's body ran, with
/// exhaustiveness (every arm's pattern set, or an explicit `else`, must
/// cover the scrutinee's whole domain) enforced by analysis, not here --
/// the parser only knows the shape.
#[derive(Debug, Clone)]
pub struct MatchExpr {
    pub scrutinee: ExpressionNode,
    pub arms: Vec<MatchArm>,
    pub else_branch: Option<CodeblockExpr>,
    pub span: Span,
}

/// `pattern => body` -- `body` is an ordinary expression (a `{ ... }`
/// codeblock is already `Expression::Codeblock`, so both a bare value and a
/// block fall out of the same `parse_expression` call; no separate "block
/// arm" shape is needed).
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: ExpressionNode,
    pub span: Span,
}

/// One arm's pattern. There is no destructuring/binding in this grammar
/// (deliberately, for now) -- a pattern only ever *proves* something about
/// the scrutinee, it never introduces new names.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// A literal (`100`, `'a'`, `true`) or an `Enum::Variant` path -- which
    /// one it is isn't decided here; analysis reads it against the
    /// scrutinee's own resolved type.
    Value(ExpressionNode),
    /// A range pattern (`RangeExpr`'s doc comment), matching a numeric
    /// scrutinee against an interval.
    Range(RangeExpr),
}

#[derive(Debug, Clone)]
pub struct FunctionCallExpr {
    pub callee: Box<ExpressionNode>,
    pub args: Vec<ExpressionNode>,
}

/// Assignment is right-associative and has the lowest precedence of any
/// expression form -- built directly as the outermost layer of expression
/// parsing (see `crate::parser::expression`), not as a generic postfix
/// operator like `FieldAccess`/`Index`/`Call`.
#[derive(Debug, Clone)]
pub struct AssignmentExpr {
    pub target: ExpressionNode,
    pub value: Box<ExpressionNode>,
}

/// `target op= value` (`+= -= *= /= %= &= |= ^= <<= >>=`) -- parses at the
/// same precedence tier as plain `=` (see `parser::expression::
/// parse_assignment`), just carrying which `BinaryOp` it desugars through.
/// Same "parser doesn't validate `target` is a place" treatment as
/// `AssignmentExpr`.
#[derive(Debug, Clone)]
pub struct CompoundAssignExpr {
    pub target: ExpressionNode,
    pub op: BinaryOp,
    pub value: Box<ExpressionNode>,
}

/// `[e1, e2, ...]` -- a fixed-size array value, one element expression per
/// slot. Unlike `Type::SizedArray`, the size isn't written down here: it's
/// just however many elements are listed, the same way `NumberExpr` doesn't
/// carry its own resolved type -- semantic analysis is what turns "N
/// elements" into a `ResolvedType::SizedArray(item, N)`.
#[derive(Debug, Clone)]
pub struct ArrayLiteralExpr {
    pub elements: Vec<ExpressionNode>,
}

/// `Name { field = value; ... }` -- builds a whole struct value (or, when the
/// path names an enum variant -- `Enum::Variant { ... }` -- an enum value) in
/// one expression, one initializer per field (analysis requires *every*
/// field to be covered exactly once). Field initializers are `;`-terminated,
/// matching the struct definition syntax they mirror, not comma-separated.
///
/// `path` is the built type's (possibly module-qualified, possibly
/// generic-argumented -- `List<u32> { ... }`, `Optional<u32>::Some { ... }`)
/// name -- kept raw like every other name at this layer; whether it actually
/// names a struct or an enum variant is analysis's question.
#[derive(Debug, Clone)]
pub struct StructLiteralExpr {
    pub path: ExprPath,
    pub fields: Vec<StructLiteralField>,
}

/// One `name: value;` initializer. `name_span` is the field name's own span,
/// so "no such field"/"field set twice" diagnostics can point at the name
/// itself rather than the whole literal.
#[derive(Debug, Clone)]
pub struct StructLiteralField {
    pub name: Ident,
    pub name_span: Span,
    pub value: ExpressionNode,
}

/// `base[range]` -- unlike a plain `Index`, this never produces a single
/// element: it produces a new slice (fat pointer) over a sub-range of
/// `base`. Parsed as a distinct postfix form from `Index` rather than
/// reusing it with an optional end bound, since the two mean entirely
/// different things (one element vs. a sub-range) and should be told apart
/// as early as possible rather than disambiguated downstream. See
/// `RangeExpr`'s doc comment for the range grammar itself.
#[derive(Debug, Clone)]
pub struct SliceExpr {
    pub base: ExpressionNode,
    pub range: RangeExpr,
}

/// `name$(arg, ...)` -- a macro invocation. Shared verbatim between all
/// three invocation positions -- expression (`Expression::MacroInvocation`,
/// usable anywhere an expression can appear), whole-statement
/// (`Statement::MacroInvocation`), and module-top-level item
/// (`Item::MacroInvocation`) -- rather than duplicated into three
/// near-identical types, since the grammar and payload shape are identical
/// in every case; only *where* the parser is wired in, and therefore which
/// grammar the expansion is re-parsed with, differs. Repetition is a
/// property of the *definition's* body, never of an argument list, so
/// `args` stays a flat list of raw token runs regardless of whether the
/// callee declares a variadic parameter. Each argument is kept as a raw
/// token slice (not parsed
/// as an `Expression`/`Type` here) since a `Type`-fragment argument (e.g.
/// `generate_type$(Counter)`) isn't valid expression syntax; see
/// `omega_parser::macros` for where each argument is validated against its
/// parameter's declared `FragmentKind` and substituted.
#[derive(Debug, Clone)]
pub struct MacroInvocationExpr {
    pub name: Ident,
    pub args: Vec<Vec<Token>>,
    /// Where the *name token* was written, which is what decides the macro
    /// environment this invocation resolves in -- the same "resolve where
    /// written" rule every other name obeys. An invocation emitted by a macro
    /// body carries that macro's expansion origin and resolves in its defining
    /// module; one that arrived inside a substituted argument keeps the
    /// caller's origin and resolves in the caller's module. Without this the
    /// two are indistinguishable after re-parsing, and a perfectly ordinary
    /// `println$("x: ", other_macro$(1, 2))` fails, because the argument's
    /// invocation gets looked up in `std::io` instead of where it was written.
    pub origin: Origin,
}
