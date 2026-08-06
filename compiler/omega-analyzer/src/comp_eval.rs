//! Compile-time expression evaluation (`comp`) -- a tree-walking
//! interpreter over an already fully-typed `CheckedExprNode`/`CheckedBlock`,
//! producing a `ConstValue`. See `docs/19-compile-time-evaluation.md`.
//!
//! Deliberately not MIR-based: MIR is a strictly later, per-function
//! lowering pass that doesn't exist yet at analysis time, and a `comp`
//! binding's value must be available *during* analysis for downstream
//! consumers (another `comp` expression referencing it, a later type
//! position). Deliberately not a second type-checker either: `comp <expr>`'s
//! inner expression is analyzed completely normally first (ordinary
//! `Analyzer::analyze_expr`, ordinary generic/overload/cross-module
//! resolution) -- this module only ever walks an already-resolved
//! `CheckedExprNode` tree, never a raw `HirExprNode`.
//!
//! Deliberately not a second, colored declaration space either: any
//! function's already-checked body can be handed to [`eval`], whether or
//! not it was ever written with `comp` in mind -- see [`CompFunctionResolver`].

use crate::checked::{
    CheckedBinaryOp, CheckedBlock, CheckedExpr, CheckedExprNode, CheckedFor, CheckedFunctionCall,
    CheckedFunctionDef, CheckedIf, CheckedLoop, CheckedMatch, CheckedPlace, CheckedPlaceRoot, CheckedProjection,
    CheckedStmt, CheckedWhile, CastKind, NumberValue, Storage,
};
use crate::resolved_type::{ConstValue, ResolvedType};
use crate::resolver::ResolveError;
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Span};
use std::collections::HashMap;

/// How far a single `comp` evaluation is allowed to run before it's assumed
/// to be a runaway compile-time loop/recursion. One shared budget for both
/// loop iterations and call depth (matches Rust's `const_eval_limit`
/// precedent) -- this alone bounds both cases uniformly, so no separate
/// call-graph cycle detector is needed on top of it.
const FUEL_LIMIT: u32 = 1_000_000;

/// What a `comp` evaluation needs from the driver: a callee's own checked
/// body, found by its already-unique `decl_id`. A generic instantiation
/// mints its own fresh synthetic id at resolution time (see
/// `ItemQueries::identity_for`/`fresh_synthetic_id` in `omega_driver`), so
/// `decl_id` alone -- with no separate module path or type-args -- is
/// already exact identity for the *specific* instantiation a call site
/// resolved to; there is nothing else to disambiguate.
pub trait CompFunctionResolver {
    /// `Ok(None)` means `decl_id` doesn't name an ordinary checked function
    /// at all (an `extern` declaration, most likely) -- distinguished from
    /// `Err` (a real resolution failure) so the interpreter can report the
    /// precise [`CompErrorKind::ExternCall`] instead of a generic failure.
    fn resolve_function_body(&mut self, decl_id: HirId) -> Result<Option<CheckedFunctionDef>, ResolveError>;
}

/// `Analyzer::analyze_comp` only ever has a `&mut dyn ModuleResolver` in
/// hand (the same handle used for every other cross-item query) -- rather
/// than making it construct some bridging adapter itself, `dyn
/// ModuleResolver` satisfies this narrower trait directly, by forwarding to
/// its own identically-shaped method. Kept as its own trait (not just a use
/// of `ModuleResolver` throughout this module) purely for testability: a
/// unit test here only ever needs to fake one method, not `ModuleResolver`'s
/// entire cross-module surface (import aliases, spec declarations, ...).
impl CompFunctionResolver for dyn crate::resolver::ModuleResolver + '_ {
    fn resolve_function_body(&mut self, decl_id: HirId) -> Result<Option<CheckedFunctionDef>, ResolveError> {
        crate::resolver::ModuleResolver::resolve_function_body(self, decl_id)
    }
}

/// Why a `comp` evaluation failed. Always paired with the [`Span`] of the
/// expression that actually blocked it, in [`CompError::span`] -- not just
/// the outermost `comp <expr>` -- so the diagnostic points at the real
/// cause even several calls deep; [`CompError::trace`] carries the call
/// chain from there back up to the outermost `comp`.
#[derive(Debug, Clone)]
pub enum CompErrorKind {
    /// Calling an `extern`-declared function -- no compile-time meaning to
    /// execute a foreign/OS call inside this interpreter.
    ExternCall,
    /// `base.method(args)` through a `spec *Spec` vtable, or an implicit
    /// concrete-to-`spec *Spec` coercion -- no compile-time meaning without
    /// real vtable data (a real gap, not silently reinterpreted).
    DynamicDispatch,
    /// Dereferencing, or projecting through, a pointer the interpreter
    /// didn't itself produce (see `ConstValue::Ref`'s doc comment) -- the
    /// interpreter never touches real memory.
    UnresolvableMemory,
    /// Reading a real runtime global (`Storage::Global`) from inside a
    /// `comp` evaluation -- only `comp`-bound identifiers (no storage, pure
    /// substitution) are readable; those never reach the interpreter as a
    /// `Storage::Global` place at all, since analysis substitutes them away
    /// before a `comp` evaluation ever sees them (see `Analyzer::
    /// analyze_comp`), so reaching this case here means the identifier is a
    /// genuine runtime place.
    NonCompGlobalRead,
    /// A local was read before it was ever assigned -- can only happen for
    /// a declared-but-uninitialized local (`a: i32;` with no initializer),
    /// since every other binding form assigns before any possible read.
    ReadBeforeInit,
    /// This call's own resolver lookup failed (the callee named a real
    /// item, but something further down the driver's own resolution
    /// failed) -- carries the resolver's own error.
    ResolutionFailed(ResolveError),
    /// Ran out of the shared fuel/depth budget -- see `FUEL_LIMIT`.
    FuelExhausted,
    /// Anything not yet supported by the interpreter -- named, not a bare
    /// "unsupported", so a diagnostic can say exactly what construct was
    /// hit (e.g. `"sizeof"`, `"a struct's dynamic enum header field"`).
    Unsupported(&'static str),
}

/// `Analyzer::analyze_comp`'s own `AnalysisErrorKind::CompEvalFailed.reason`
/// is built straight from this, matching how every other findable-facts
/// enum in this crate (`ResolveError`, ...) renders itself.
impl std::fmt::Display for CompErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExternCall => write!(f, "it calls an 'extern' function"),
            Self::DynamicDispatch => write!(f, "it uses dynamic dispatch through a 'spec' object"),
            Self::UnresolvableMemory => write!(f, "it dereferences a pointer this evaluation didn't itself produce"),
            Self::NonCompGlobalRead => write!(f, "it reads a global that isn't itself a 'comp' binding"),
            Self::ReadBeforeInit => write!(f, "it reads a local before it's ever assigned"),
            Self::ResolutionFailed(e) => write!(f, "{e}"),
            Self::FuelExhausted => write!(f, "it ran for too long (a runaway loop or recursion)"),
            Self::Unsupported(what) => write!(f, "{what} isn't supported in a compile-time evaluation yet"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompError {
    pub kind: CompErrorKind,
    pub span: Span,
    /// The call-site spans between the outermost `comp <expr>` and `span`'s
    /// own call frame, outermost first -- empty when the failure happened
    /// directly inside the outermost evaluation, with no intervening call.
    pub trace: Vec<Span>,
}

/// A control-flow signal a statement/block can produce, unwound through the
/// interpreter's own recursion via `?` (see `Outcome`) exactly like
/// `return`/`break`/`continue` unwind through an ordinary tree-walking
/// interpreter -- the direct analogue of the real reason MIR lowering
/// flattens these into an explicit block graph instead (see `omega_mir::
/// body`'s module doc comment): this interpreter has no block graph to jump
/// around in, so the same control transfer is modeled as an explicit signal
/// instead.
enum Signal {
    Return(ConstValue),
    Break,
    Continue,
}

/// Either a real evaluation failure or an in-flight control-flow signal --
/// unified so ordinary `?` propagates both uniformly through expression
/// evaluation (a `{ return x; }` block used as an operand has to unwind
/// through arithmetic, field access, anything, exactly like it does in a
/// real function body). Only a loop's own body evaluation (`Break`/
/// `Continue`) and a call's own body evaluation (`Return`) ever catch an
/// `Outcome::Signal` instead of propagating it further.
enum Outcome {
    Error(CompError),
    Signal(Signal),
}

impl From<CompError> for Outcome {
    fn from(e: CompError) -> Self {
        Outcome::Error(e)
    }
}

type CompResult<T> = Result<T, Outcome>;

/// One call frame's locals -- parameters and declared locals share one
/// space, keyed by `HirId`, exactly like `MirBody::locals`' unified index
/// space does for the identical reason (codegen/interpretation both treat a
/// parameter and a declared local identically once bound).
#[derive(Default)]
struct Frame {
    locals: HashMap<HirId, ConstValue>,
    /// Every `defer`'s own body, in the order each was *reached* (not
    /// lexical/declaration order -- a `defer` inside a conditional branch
    /// only queues if that branch actually runs) -- run in reverse (FILO)
    /// when this frame's own function exits, whether via `return` or
    /// falling off the end, matching this language's real `defer`
    /// semantics (see `docs/00-functions.md`'s `defer` section): scoped to
    /// the whole *function*, not the block a `defer` statement happens to
    /// sit in, so this lives on the frame, not on any block-local state.
    /// Cloned out of the checked tree at the point each `defer` runs (see
    /// `Interpreter::eval_stmt`'s own `Defer` arm) since nothing here
    /// borrows the callee's `CheckedFunctionDef` past its own call.
    defers: Vec<CheckedBlock>,
}

struct Interpreter<'r, R: CompFunctionResolver + ?Sized> {
    resolver: &'r mut R,
    fuel: u32,
    frames: Vec<Frame>,
    /// Call-site spans, outermost first -- pushed on entry to
    /// `eval_function_call`, popped on return, and copied into a
    /// [`CompError::trace`] at the point an error is first raised (see
    /// `Self::err`).
    call_trace: Vec<Span>,
}

/// Evaluates `expr` at compile time -- the whole crate's one public entry
/// point, called from `Analyzer::analyze_comp` once `expr`'s inner
/// expression has already been fully, ordinarily type-checked. Generic
/// (rather than `&mut dyn CompFunctionResolver` directly) so the ordinary
/// `&mut dyn ModuleResolver` handle `Analyzer` already carries -- which
/// satisfies `CompFunctionResolver` via the blanket impl above -- can be
/// passed straight through: `&mut dyn ModuleResolver` coercing to `&mut dyn
/// CompFunctionResolver` directly isn't something Rust does between two
/// unrelated trait objects, but instantiating `R = dyn ModuleResolver` here
/// needs no coercion at all, just the ordinary blanket impl.
pub fn eval<R: CompFunctionResolver + ?Sized>(resolver: &mut R, expr: &CheckedExprNode) -> Result<ConstValue, CompError> {
    let mut interp = Interpreter { resolver, fuel: FUEL_LIMIT, frames: vec![Frame::default()], call_trace: vec![] };
    let result = interp.eval_expr(expr);
    // A `defer` reached directly by the outermost `comp <expr>` (rather
    // than inside a function `call_function` itself calls into, which
    // already drains its own frame's defers on the way out -- see its own
    // doc comment) has no function-call boundary of its own to run at the
    // end of; this frame is the closest thing to one. Same "only once the
    // value itself evaluated cleanly" rule `call_function` applies.
    let result = match result {
        Ok(value) => {
            let defers = std::mem::take(&mut interp.frame().defers);
            let mut result = Ok(value);
            for deferred in defers.into_iter().rev() {
                if let Err(e) = interp.eval_block(&deferred) {
                    result = Err(e);
                    break;
                }
            }
            result
        }
        Err(other) => Err(other),
    };
    match result {
        Ok(value) => Ok(value),
        Err(Outcome::Error(e)) => Err(e),
        // A bare `return`/`break`/`continue` reaching the outermost `comp
        // <expr>` with no enclosing call/loop to catch it is impossible in
        // a validly checked tree (analysis already rejects `return` outside
        // a function, `break`/`continue` outside a loop) -- if it happens
        // anyway, that's an analyzer/interpreter bug, not a user-reportable
        // `comp` failure.
        Err(Outcome::Signal(_)) => unreachable!("control-flow signal escaped the outermost comp evaluation"),
    }
}

impl<'r, R: CompFunctionResolver + ?Sized> Interpreter<'r, R> {
    fn err(&self, span: Span, kind: CompErrorKind) -> Outcome {
        Outcome::Error(CompError { kind, span, trace: self.call_trace.clone() })
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("comp evaluation always has at least one frame")
    }

    fn tick(&mut self, span: Span) -> CompResult<()> {
        if self.fuel == 0 {
            return Err(self.err(span, CompErrorKind::FuelExhausted));
        }
        self.fuel -= 1;
        Ok(())
    }

    // ---- expressions ----------------------------------------------------

    fn eval_expr(&mut self, node: &CheckedExprNode) -> CompResult<ConstValue> {
        self.tick(node.span)?;
        match &node.kind {
            CheckedExpr::Place(place) => self.read_place(place, node.span),
            CheckedExpr::Number(n) => Ok(ConstValue::Number(*n)),
            CheckedExpr::Bool(b) => Ok(ConstValue::Bool(*b)),
            CheckedExpr::Char(c) => Ok(ConstValue::Char(*c)),
            CheckedExpr::String(s) => Ok(ConstValue::Str(s.clone())),
            // No dedicated byte-string `ConstValue` shape -- represented as
            // an ordinary `Slice` of `U8` `Number`s, matching how every
            // other `comp`-evaluated slice is represented.
            CheckedExpr::ByteString(s) => {
                Ok(ConstValue::Slice(s.bytes().map(|b| ConstValue::Number(NumberValue::Unsigned(b as u64))).collect()))
            }
            // Already a fully evaluated constant -- an enum tag/header
            // value, a `&[...]` literal, or (see `Analyzer::analyze_comp`)
            // a previously-evaluated `comp <expr>` reached again through
            // another comp evaluation (e.g. a `comp` binding's own value,
            // substituted in at every use site).
            CheckedExpr::Const(value) => Ok(value.clone()),
            CheckedExpr::FunctionCall(call) => self.eval_call(call, node.span),
            CheckedExpr::Assignment(assign) => {
                let value = self.eval_expr(&assign.value)?;
                self.write_place(&assign.target, value.clone(), node.span)?;
                Ok(value)
            }
            CheckedExpr::AddressOf(addr) => {
                let value = self.read_place(&addr.place, node.span)?;
                Ok(ConstValue::Ref(Box::new(value)))
            }
            CheckedExpr::Negate(inner) => self.eval_negate(inner, node.span),
            CheckedExpr::BitNot(inner) => self.eval_bitnot(inner, node.span),
            CheckedExpr::BinaryOp(bin) => self.eval_binary_op(bin, node.span),
            CheckedExpr::Codeblock(block) => match self.eval_block(block)? {
                BlockResult::Value(v) => Ok(v),
                BlockResult::Diverged => Ok(ConstValue::Bool(false)), // unreachable tail; type is Void/never here
            },
            CheckedExpr::If(if_expr) => self.eval_if(if_expr, node.span),
            CheckedExpr::Match(match_expr) => self.eval_match(match_expr, node.span),
            CheckedExpr::ArrayLiteral(arr) => {
                let mut values = Vec::with_capacity(arr.elements.len());
                for element in &arr.elements {
                    values.push(self.eval_expr(element)?);
                }
                Ok(ConstValue::Array(values))
            }
            CheckedExpr::StructLiteral(lit) => {
                // `fields` is already sorted into `field_index` order by
                // analysis (see `CheckedStructLiteral`'s doc comment) --
                // evaluated in that same (source) order for side effects,
                // then stored positionally.
                let mut values: Vec<Option<ConstValue>> = (0..lit.fields.len()).map(|_| None).collect();
                for field in &lit.fields {
                    let value = self.eval_expr(&field.value)?;
                    if field.field_index >= values.len() {
                        values.resize(field.field_index + 1, None);
                    }
                    values[field.field_index] = Some(value);
                }
                let fields = values
                    .into_iter()
                    .map(|v| v.expect("analysis guarantees every declared field is initialized exactly once"))
                    .collect();
                Ok(ConstValue::Struct(fields))
            }
            CheckedExpr::EnumConstruct(construct) => {
                let (tag, header, dynamic_count) = self.enum_variant_facts(node, construct.variant_index)?;
                // `construct.fields` is in *source* (evaluation) order, each
                // entry carrying its *declared* position in the combined
                // "dynamic fields, then this variant's own body fields"
                // list (see `CheckedEnumConstruct`'s doc comment) -- values
                // are evaluated in source order (their side effects must
                // run in that order) but stored positionally, exactly like
                // `CheckedExpr::StructLiteral` just above.
                let mut values: Vec<Option<ConstValue>> = (0..construct.fields.len()).map(|_| None).collect();
                for field in &construct.fields {
                    let value = self.eval_expr(&field.value)?;
                    if field.field_index >= values.len() {
                        values.resize(field.field_index + 1, None);
                    }
                    values[field.field_index] = Some(value);
                }
                let mut values: Vec<ConstValue> = values
                    .into_iter()
                    .map(|v| v.expect("analysis guarantees every declared field is initialized exactly once"))
                    .collect();
                let fields = values.split_off(dynamic_count);
                Ok(ConstValue::Enum { variant_index: construct.variant_index, tag, header, dynamic_fields: values, fields })
            }
            CheckedExpr::UnionConstruct(construct) => {
                let value = self.eval_expr(&construct.value)?;
                Ok(ConstValue::Union { field_index: construct.field_index, value: Box::new(value) })
            }
            CheckedExpr::Slice(slice) => self.eval_slice(slice, node.span),
            CheckedExpr::Cast(cast) => self.eval_cast(cast, node.span),
            // Pointer-width-independent: this compiler targets exactly one
            // pointer width today (see `ResolvedType::numeric_kind`'s
            // identical `ISize`/`USize` hardcoding), so `sizeof` inside a
            // `comp` evaluation uses that same fixed width rather than
            // threading a real target through the interpreter.
            CheckedExpr::Sizeof(target) => {
                Ok(ConstValue::Number(NumberValue::Unsigned(crate::layout::total_bytes(target, 8) as u64)))
            }
            CheckedExpr::SpecCoerce(_) => Err(self.err(node.span, CompErrorKind::DynamicDispatch)),
            CheckedExpr::DynamicCall(_) => Err(self.err(node.span, CompErrorKind::DynamicDispatch)),
            // No `ConstValue` shape represents a fat pointer built from an
            // arbitrary runtime address -- unlike `sizeof`, this has no
            // pointer-width-independent meaning at compile time.
            CheckedExpr::RawSlice(_) => Err(self.err(node.span, CompErrorKind::Unsupported("raw_slice"))),
        }
    }

    /// The facts a fresh `EnumConstruct` needs -- read directly off the
    /// enum's shared resolved cell (`node.r#type` is always `ResolvedType::
    /// Enum { cell, variant: Some(variant_index) }` for an `EnumConstruct`
    /// node, per `CheckedExpr::EnumConstruct`'s own doc comment), the one
    /// point where the interpreter needs a `ResolvedType`, not just a
    /// `ConstValue`, in scope: the tag, a clone of the variant's own
    /// per-variant header constants (see `ConstValue::Enum`'s doc comment
    /// on why this is duplicated here rather than re-read from the cell at
    /// every later access), and how many of `CheckedEnumConstruct::fields`'
    /// combined list are shared dynamic fields (the rest are this
    /// variant's own body fields) -- see that type's doc comment.
    fn enum_variant_facts(
        &self,
        node: &CheckedExprNode,
        variant_index: usize,
    ) -> CompResult<(NumberValue, Vec<ConstValue>, usize)> {
        match &node.r#type {
            ResolvedType::Enum { cell, .. } => {
                let cell = cell.borrow();
                let variant = &cell.variants[variant_index];
                Ok((variant.tag, variant.header_values.clone(), cell.dynamic_fields.len()))
            }
            _ => unreachable!("CheckedExpr::EnumConstruct's own type is always ResolvedType::Enum"),
        }
    }

    fn eval_negate(&mut self, inner: &CheckedExprNode, span: Span) -> CompResult<ConstValue> {
        match self.eval_expr(inner)? {
            ConstValue::Number(NumberValue::Signed(n)) => Ok(ConstValue::Number(NumberValue::Signed(n.wrapping_neg()))),
            ConstValue::Number(NumberValue::Float(f)) => Ok(ConstValue::Number(NumberValue::Float(-f))),
            _ => Err(self.err(span, CompErrorKind::Unsupported("negation of a non-numeric comp value"))),
        }
    }

    fn eval_bitnot(&mut self, inner: &CheckedExprNode, span: Span) -> CompResult<ConstValue> {
        match self.eval_expr(inner)? {
            ConstValue::Number(NumberValue::Signed(n)) => Ok(ConstValue::Number(NumberValue::Signed(!n))),
            ConstValue::Number(NumberValue::Unsigned(n)) => Ok(ConstValue::Number(NumberValue::Unsigned(!n))),
            ConstValue::Bool(b) => Ok(ConstValue::Bool(!b)),
            _ => Err(self.err(span, CompErrorKind::Unsupported("bitwise-not of a non-integer comp value"))),
        }
    }

    fn eval_binary_op(&mut self, bin: &CheckedBinaryOp, span: Span) -> CompResult<ConstValue> {
        let left = self.eval_expr(&bin.left)?;
        let right = self.eval_expr(&bin.right)?;
        // Analysis already guarantees both operands share one resolved
        // numeric type (see `CheckedBinaryOp`'s doc comment) -- the
        // interpreter only has to pick signed/unsigned/float arithmetic
        // from the *values'* own shape, never re-check agreement.
        match (left, right) {
            (ConstValue::Number(l), ConstValue::Number(r)) => self.eval_numeric_binary_op(bin.op, l, r, span),
            (ConstValue::Bool(l), ConstValue::Bool(r)) => self.eval_bool_binary_op(bin.op, l, r, span),
            (ConstValue::Char(l), ConstValue::Char(r)) => self.eval_char_binary_op(bin.op, l, r, span),
            _ => Err(self.err(span, CompErrorKind::Unsupported("binary operator on this comp value shape"))),
        }
    }

    fn eval_bool_binary_op(&mut self, op: BinaryOp, l: bool, r: bool, span: Span) -> CompResult<ConstValue> {
        match op {
            BinaryOp::Eq => Ok(ConstValue::Bool(l == r)),
            BinaryOp::Ne => Ok(ConstValue::Bool(l != r)),
            BinaryOp::BitAnd => Ok(ConstValue::Bool(l & r)),
            BinaryOp::BitOr => Ok(ConstValue::Bool(l | r)),
            BinaryOp::BitXor => Ok(ConstValue::Bool(l ^ r)),
            _ => Err(self.err(span, CompErrorKind::Unsupported("this operator on bool"))),
        }
    }

    fn eval_char_binary_op(&mut self, op: BinaryOp, l: char, r: char, span: Span) -> CompResult<ConstValue> {
        if op.is_comparison() {
            let ord = (l as u32).cmp(&(r as u32));
            return Ok(ConstValue::Bool(compare(op, ord)));
        }
        Err(self.err(span, CompErrorKind::Unsupported("arithmetic on char")))
    }

    fn eval_numeric_binary_op(&mut self, op: BinaryOp, l: NumberValue, r: NumberValue, span: Span) -> CompResult<ConstValue> {
        use NumberValue::*;
        if op.is_comparison() {
            let ord = match (l, r) {
                (Signed(l), Signed(r)) => l.cmp(&r),
                (Unsigned(l), Unsigned(r)) => l.cmp(&r),
                (Float(l), Float(r)) => match l.partial_cmp(&r) {
                    Some(ord) => ord,
                    // NaN: every ordered comparison is false, `!=` is true
                    // -- `Eq`/`Ne` are handled separately, below, so this
                    // arm only ever needs to make `<`/`<=`/`>`/`>=` false.
                    None => return Ok(ConstValue::Bool(false)),
                },
                _ => return Err(self.err(span, CompErrorKind::Unsupported("comparison across numeric kinds"))),
            };
            if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && let (Float(l), Float(r)) = (l, r) {
                return Ok(ConstValue::Bool(if op == BinaryOp::Eq { l == r } else { l != r }));
            }
            return Ok(ConstValue::Bool(compare(op, ord)));
        }
        match (l, r) {
            (Signed(l), Signed(r)) => self.eval_signed_arith(op, l, r, span),
            (Unsigned(l), Unsigned(r)) => self.eval_unsigned_arith(op, l, r, span),
            (Float(l), Float(r)) => self.eval_float_arith(op, l, r, span),
            _ => Err(self.err(span, CompErrorKind::Unsupported("arithmetic across numeric kinds"))),
        }
    }

    fn eval_signed_arith(&mut self, op: BinaryOp, l: i64, r: i64, span: Span) -> CompResult<ConstValue> {
        let v = match op {
            BinaryOp::Add => l.wrapping_add(r),
            BinaryOp::Sub => l.wrapping_sub(r),
            BinaryOp::Mul => l.wrapping_mul(r),
            BinaryOp::Div if r == 0 => return Err(self.err(span, CompErrorKind::Unsupported("division by zero"))),
            BinaryOp::Div => l.wrapping_div(r),
            BinaryOp::Rem if r == 0 => return Err(self.err(span, CompErrorKind::Unsupported("division by zero"))),
            BinaryOp::Rem => l.wrapping_rem(r),
            BinaryOp::BitAnd => l & r,
            BinaryOp::BitOr => l | r,
            BinaryOp::BitXor => l ^ r,
            BinaryOp::Shl => l.wrapping_shl(r as u32),
            BinaryOp::Shr => l.wrapping_shr(r as u32),
            _ => return Err(self.err(span, CompErrorKind::Unsupported("this operator on a signed integer"))),
        };
        Ok(ConstValue::Number(NumberValue::Signed(v)))
    }

    fn eval_unsigned_arith(&mut self, op: BinaryOp, l: u64, r: u64, span: Span) -> CompResult<ConstValue> {
        let v = match op {
            BinaryOp::Add => l.wrapping_add(r),
            BinaryOp::Sub => l.wrapping_sub(r),
            BinaryOp::Mul => l.wrapping_mul(r),
            BinaryOp::Div if r == 0 => return Err(self.err(span, CompErrorKind::Unsupported("division by zero"))),
            BinaryOp::Div => l.wrapping_div(r),
            BinaryOp::Rem if r == 0 => return Err(self.err(span, CompErrorKind::Unsupported("division by zero"))),
            BinaryOp::Rem => l.wrapping_rem(r),
            BinaryOp::BitAnd => l & r,
            BinaryOp::BitOr => l | r,
            BinaryOp::BitXor => l ^ r,
            BinaryOp::Shl => l.wrapping_shl(r as u32),
            BinaryOp::Shr => l.wrapping_shr(r as u32),
            _ => return Err(self.err(span, CompErrorKind::Unsupported("this operator on an unsigned integer"))),
        };
        Ok(ConstValue::Number(NumberValue::Unsigned(v)))
    }

    fn eval_float_arith(&mut self, op: BinaryOp, l: f64, r: f64, span: Span) -> CompResult<ConstValue> {
        let v = match op {
            BinaryOp::Add => l + r,
            BinaryOp::Sub => l - r,
            BinaryOp::Mul => l * r,
            BinaryOp::Div => l / r,
            BinaryOp::Rem => l % r,
            _ => return Err(self.err(span, CompErrorKind::Unsupported("this operator on a float"))),
        };
        Ok(ConstValue::Number(NumberValue::Float(v)))
    }

    fn eval_cast(&mut self, cast: &crate::checked::CheckedCast, span: Span) -> CompResult<ConstValue> {
        let base = self.eval_expr(&cast.base)?;
        match cast.kind {
            // Same underlying representation on both sides -- including the
            // str/byte-slice family's "both leaves, unchanged" case, which
            // needs no per-shape handling here since the `ConstValue` is
            // simply carried through as-is either way.
            CastKind::Reinterpret => Ok(base),
            // `*str`/`*[T]` (fat, `[ptr, len]`) -> `*u8`/`*T` (thin) --
            // keeps the pointer leaf, discards the length, exactly like
            // ordinary (non-`comp`) `DropLength` codegen does. Represented
            // as `&<the raw bytes/elements, as an inline Array>`: `Ref`
            // already means "address of a separately-built piece of comp
            // data" (see its doc comment), and an `Array`'s own leaves are
            // already written inline with no indirection of their own --
            // exactly the byte layout a thin pointer's pointee has. Doesn't
            // alias the *same* data object the fat-pointer form would use
            // (a fresh one is built instead) -- harmless: nothing needs
            // this pointer to be reference-equal to another, only valid.
            CastKind::DropLength => match base {
                ConstValue::Str(s) => {
                    let bytes = s.bytes().map(|b| ConstValue::Number(NumberValue::Unsigned(b as u64))).collect();
                    Ok(ConstValue::Ref(Box::new(ConstValue::Array(bytes))))
                }
                ConstValue::Slice(elements) => Ok(ConstValue::Ref(Box::new(ConstValue::Array(elements)))),
                _ => Err(self.err(span, CompErrorKind::Unsupported("a fat-to-thin pointer cast of a non-str/slice comp value"))),
            },
            _ => {
                let ConstValue::Number(n) = base else {
                    return Err(self.err(span, CompErrorKind::Unsupported("a numeric cast of a non-numeric comp value")));
                };
                let Some(target) = cast.target_type.numeric_kind() else {
                    return Err(self.err(span, CompErrorKind::Unsupported("a cast to a non-numeric type")));
                };
                Ok(ConstValue::Number(cast_number(n, target)))
            }
        }
    }

    fn eval_slice(&mut self, slice: &crate::checked::CheckedSlice, span: Span) -> CompResult<ConstValue> {
        let base = self.read_place(&slice.base, span)?;
        let elements = match base {
            ConstValue::Array(v) | ConstValue::Slice(v) => v,
            _ => return Err(self.err(span, CompErrorKind::Unsupported("slicing a non-array/slice comp value"))),
        };
        let start = match &slice.start {
            Some(e) => self.expect_index(e)?,
            None => 0,
        };
        let end = match &slice.end {
            Some(e) => {
                let i = self.expect_index(e)?;
                if slice.inclusive { i + 1 } else { i }
            }
            None => elements.len(),
        };
        if start > end || end > elements.len() {
            return Err(self.err(span, CompErrorKind::Unsupported("an out-of-range comp slice")));
        }
        Ok(ConstValue::Slice(elements[start..end].to_vec()))
    }

    fn expect_index(&mut self, node: &CheckedExprNode) -> CompResult<usize> {
        match self.eval_expr(node)? {
            ConstValue::Number(NumberValue::Signed(n)) if n >= 0 => Ok(n as usize),
            ConstValue::Number(NumberValue::Unsigned(n)) => Ok(n as usize),
            _ => Err(self.err(node.span, CompErrorKind::Unsupported("a non-integer index"))),
        }
    }

    fn eval_if(&mut self, if_expr: &CheckedIf, span: Span) -> CompResult<ConstValue> {
        for (condition, body) in &if_expr.branches {
            match self.eval_expr(condition)? {
                ConstValue::Bool(true) => {
                    return match self.eval_block(body)? {
                        BlockResult::Value(v) => Ok(v),
                        BlockResult::Diverged => Ok(ConstValue::Bool(false)),
                    };
                }
                ConstValue::Bool(false) => continue,
                _ => return Err(self.err(span, CompErrorKind::Unsupported("a non-bool if-condition"))),
            }
        }
        match &if_expr.else_branch {
            Some(body) => match self.eval_block(body)? {
                BlockResult::Value(v) => Ok(v),
                BlockResult::Diverged => Ok(ConstValue::Bool(false)),
            },
            None => Ok(ConstValue::Bool(false)), // Void: no branch taken, nothing to produce
        }
    }

    fn eval_match(&mut self, match_expr: &CheckedMatch, span: Span) -> CompResult<ConstValue> {
        'arms: for arm in &match_expr.arms {
            for condition in &arm.conditions {
                match self.eval_expr(condition)? {
                    ConstValue::Bool(true) => {}
                    ConstValue::Bool(false) => continue 'arms,
                    _ => return Err(self.err(span, CompErrorKind::Unsupported("a non-bool match condition"))),
                }
            }
            return match self.eval_block(&arm.body)? {
                BlockResult::Value(v) => Ok(v),
                BlockResult::Diverged => Ok(ConstValue::Bool(false)),
            };
        }
        match &match_expr.else_branch {
            Some(body) => match self.eval_block(body)? {
                BlockResult::Value(v) => Ok(v),
                BlockResult::Diverged => Ok(ConstValue::Bool(false)),
            },
            // Exhaustiveness was already proven by analysis -- falling off
            // the end here means either a checked-tree invariant broke, or
            // (far more likely) analysis proved coverage using information
            // (a real enum discriminant only known here, mid-evaluation)
            // this interpreter's own arm evaluation didn't reproduce.
            None => Err(self.err(span, CompErrorKind::Unsupported("an exhaustive match with no matching arm"))),
        }
    }

    // ---- calls ------------------------------------------------------------

    fn eval_call(&mut self, call: &CheckedFunctionCall, span: Span) -> CompResult<ConstValue> {
        let CheckedExpr::Place(CheckedPlace { root: CheckedPlaceRoot::Variable { decl_id, storage: Storage::Function, .. }, projections }) =
            &call.callee.kind
        else {
            return Err(self.err(span, CompErrorKind::Unsupported("an indirect call")));
        };
        debug_assert!(projections.is_empty(), "a Storage::Function place is never itself projected");

        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.eval_expr(arg)?);
        }

        let body = match self.resolver.resolve_function_body(*decl_id) {
            Ok(Some(body)) => body,
            Ok(None) => return Err(self.err(span, CompErrorKind::ExternCall)),
            Err(error) => return Err(self.err(span, CompErrorKind::ResolutionFailed(error))),
        };

        self.tick(span)?;
        self.call_trace.push(span);
        let result = self.call_function(&body, args);
        self.call_trace.pop();
        result.map_err(|outcome| match outcome {
            // A `return` at the callee's own top level is ordinary control
            // flow, caught right here -- never a signal that should keep
            // unwinding into the *caller's* own frame.
            Outcome::Signal(Signal::Return(_)) => {
                unreachable!("call_function always converts its own Return signal into a value")
            }
            other => other,
        })
    }

    fn call_function(&mut self, body: &CheckedFunctionDef, args: Vec<ConstValue>) -> CompResult<ConstValue> {
        let mut frame = Frame::default();
        for (param, value) in body.params.iter().zip(args) {
            frame.locals.insert(param.id, value);
        }
        self.frames.push(frame);
        let value = match self.eval_block(&body.body) {
            Ok(BlockResult::Value(v)) => Ok(v),
            // `Void`-returning function falling off the end with no tail --
            // matches `CheckedBlock::tail`'s own "no trailing expression"
            // convention; there is no meaningful value to hand back.
            Ok(BlockResult::Diverged) => Ok(ConstValue::Bool(false)),
            Err(Outcome::Signal(Signal::Return(v))) => Ok(v),
            Err(other) => Err(other),
        };
        // Defers run only once the body itself finished evaluating cleanly
        // (fell through, or hit an ordinary `return`) -- in FILO order,
        // matching this language's real `defer` semantics. If the body
        // failed instead, there's nothing for a defer to be cleaning up
        // after: the whole comp evaluation has already failed regardless
        // of what a deferred block might otherwise have done, so they're
        // skipped rather than run against a value that was never produced.
        let result = match value {
            Ok(v) => {
                let defers = std::mem::take(&mut self.frame().defers);
                let mut result = Ok(v);
                for deferred in defers.into_iter().rev() {
                    if let Err(e) = self.eval_block(&deferred) {
                        result = Err(e);
                        break;
                    }
                }
                result
            }
            Err(other) => Err(other),
        };
        self.frames.pop();
        result
    }

    // ---- statements/blocks --------------------------------------------

    fn eval_block(&mut self, block: &CheckedBlock) -> CompResult<BlockResult> {
        for stmt in &block.stmts {
            self.eval_stmt(stmt)?;
        }
        match &block.tail {
            Some(tail) => Ok(BlockResult::Value(self.eval_expr(tail)?)),
            None => Ok(BlockResult::Diverged),
        }
    }

    fn eval_stmt(&mut self, stmt: &CheckedStmt) -> CompResult<()> {
        match stmt {
            // No initializer to run -- either a genuinely uninitialized
            // local (a later read is `ReadBeforeInit`) or immediately
            // followed by its own desugared `Assignment` statement (see
            // `Analyzer::analyze_walrus`).
            CheckedStmt::Declaration(_) => Ok(()),
            CheckedStmt::ExternDeclaration(_) => Ok(()),
            CheckedStmt::Expression(expr) => {
                self.eval_expr(expr)?;
                Ok(())
            }
            CheckedStmt::Return(expr) => {
                let value = self.eval_expr(expr)?;
                Err(Outcome::Signal(Signal::Return(value)))
            }
            CheckedStmt::While(w) => self.eval_while(w),
            CheckedStmt::Loop(l) => self.eval_loop(l),
            CheckedStmt::For(f) => self.eval_for(f),
            CheckedStmt::Break(_) => Err(Outcome::Signal(Signal::Break)),
            CheckedStmt::Continue(_) => Err(Outcome::Signal(Signal::Continue)),
            // Queued on the *frame* (function-scoped, not block-scoped --
            // see `Frame::defers`' doc comment), not run here -- `call_
            // function` runs every queued defer, in FILO order, once this
            // frame's own function body finishes evaluating.
            CheckedStmt::Defer(d) => {
                self.frame().defers.push(d.body.clone());
                Ok(())
            }
        }
    }

    fn eval_while(&mut self, w: &CheckedWhile) -> CompResult<()> {
        loop {
            self.tick(w.span)?;
            match self.eval_expr(&w.condition)? {
                ConstValue::Bool(true) => {}
                ConstValue::Bool(false) => return Ok(()),
                _ => return Err(self.err(w.span, CompErrorKind::Unsupported("a non-bool while-condition"))),
            }
            match self.eval_block(&w.body) {
                Ok(_) => {}
                Err(Outcome::Signal(Signal::Break)) => return Ok(()),
                Err(Outcome::Signal(Signal::Continue)) => continue,
                Err(other) => return Err(other),
            }
        }
    }

    /// `loop { body }` -- see `eval_while`; identical except there's no
    /// condition to evaluate each iteration, so the only way out is a
    /// `break` (or exhausting `self.tick`'s own budget, same backstop
    /// `eval_while`/`eval_for` already rely on for a genuinely unbounded
    /// `comp`-time loop).
    fn eval_loop(&mut self, l: &CheckedLoop) -> CompResult<()> {
        loop {
            self.tick(l.span)?;
            match self.eval_block(&l.body) {
                Ok(_) => {}
                Err(Outcome::Signal(Signal::Break)) => return Ok(()),
                Err(Outcome::Signal(Signal::Continue)) => continue,
                Err(other) => return Err(other),
            }
        }
    }

    fn eval_for(&mut self, f: &CheckedFor) -> CompResult<()> {
        for init in &f.init {
            self.eval_stmt(init)?;
        }
        loop {
            self.tick(f.span)?;
            match self.eval_expr(&f.condition)? {
                ConstValue::Bool(true) => {}
                ConstValue::Bool(false) => return Ok(()),
                _ => return Err(self.err(f.span, CompErrorKind::Unsupported("a non-bool for-condition"))),
            }
            match self.eval_block(&f.body) {
                Ok(_) => {}
                Err(Outcome::Signal(Signal::Break)) => return Ok(()),
                Err(Outcome::Signal(Signal::Continue)) => {}
                Err(other) => return Err(other),
            }
            if let Some(post) = &f.post {
                self.eval_expr(post)?;
            }
        }
    }

    // ---- places -----------------------------------------------------------

    fn read_place(&mut self, place: &CheckedPlace, span: Span) -> CompResult<ConstValue> {
        let mut value = self.read_root(&place.root, span)?;
        for proj in &place.projections {
            value = self.read_projection(value, proj, span)?;
        }
        Ok(value)
    }

    fn read_root(&mut self, root: &CheckedPlaceRoot, span: Span) -> CompResult<ConstValue> {
        match root {
            CheckedPlaceRoot::Variable { decl_id, storage: Storage::Local | Storage::Parameter, .. } => self
                .frame()
                .locals
                .get(decl_id)
                .cloned()
                .ok_or_else(|| self.err(span, CompErrorKind::ReadBeforeInit)),
            CheckedPlaceRoot::Variable { storage: Storage::Global, .. } => {
                Err(self.err(span, CompErrorKind::NonCompGlobalRead))
            }
            CheckedPlaceRoot::Variable { storage: Storage::Function, .. } => {
                Err(self.err(span, CompErrorKind::Unsupported("a function value used outside of a direct call")))
            }
            CheckedPlaceRoot::Variable { storage: Storage::Comp, .. } => {
                unreachable!("a comp binding is substituted into CheckedExpr::Const during analysis -- see Storage::Comp's doc comment")
            }
            CheckedPlaceRoot::Expr(expr) => self.eval_expr(expr),
        }
    }

    fn read_projection(&mut self, value: ConstValue, proj: &CheckedProjection, span: Span) -> CompResult<ConstValue> {
        match proj {
            CheckedProjection::FieldAccess { index, .. } => match value {
                ConstValue::Struct(fields) => Ok(fields[*index].clone()),
                _ => Err(self.err(span, CompErrorKind::Unsupported("field access on a non-struct comp value"))),
            },
            CheckedProjection::Index { index_expr, .. } => {
                let index = self.expect_index(index_expr)?;
                match value {
                    ConstValue::Array(v) | ConstValue::Slice(v) => v
                        .get(index)
                        .cloned()
                        .ok_or_else(|| self.err(span, CompErrorKind::Unsupported("an out-of-range comp index"))),
                    _ => Err(self.err(span, CompErrorKind::Unsupported("indexing a non-array/slice comp value"))),
                }
            }
            CheckedProjection::Deref { .. } => match value {
                ConstValue::Ref(inner) => Ok(*inner),
                _ => Err(self.err(span, CompErrorKind::UnresolvableMemory)),
            },
            CheckedProjection::SliceLength => match value {
                ConstValue::Slice(v) | ConstValue::Array(v) => Ok(ConstValue::Number(NumberValue::Unsigned(v.len() as u64))),
                ConstValue::Str(s) => Ok(ConstValue::Number(NumberValue::Unsigned(s.len() as u64))),
                _ => Err(self.err(span, CompErrorKind::Unsupported("length of a non-slice/str comp value"))),
            },
            CheckedProjection::EnumTag { .. } => match value {
                ConstValue::Enum { tag, .. } => Ok(ConstValue::Number(tag)),
                _ => Err(self.err(span, CompErrorKind::Unsupported("tag access on a non-enum comp value"))),
            },
            CheckedProjection::EnumBody { field_index, .. } => match value {
                ConstValue::Enum { fields, .. } => Ok(fields[*field_index].clone()),
                _ => Err(self.err(span, CompErrorKind::Unsupported("body-field access on a non-enum comp value"))),
            },
            CheckedProjection::EnumHeader { index, .. } => match value {
                ConstValue::Enum { header, .. } => Ok(header[*index].clone()),
                _ => Err(self.err(span, CompErrorKind::Unsupported("header access on a non-enum comp value"))),
            },
            CheckedProjection::EnumDynamicField { index, .. } => match value {
                ConstValue::Enum { dynamic_fields, .. } => Ok(dynamic_fields[*index].clone()),
                _ => Err(self.err(span, CompErrorKind::Unsupported("dynamic-field access on a non-enum comp value"))),
            },
            CheckedProjection::UnionField { index, .. } => match value {
                ConstValue::Union { field_index, value } if field_index == *index => Ok(*value),
                ConstValue::Union { .. } => Err(self.err(span, CompErrorKind::Unsupported("reading a union through its inactive field"))),
                _ => Err(self.err(span, CompErrorKind::Unsupported("field access on a non-union comp value"))),
            },
            // A `spec *Self` value has no `ConstValue` shape -- dynamic
            // dispatch isn't comp-evaluable, so this can never actually see
            // a real base value; reject uniformly rather than panic.
            CheckedProjection::SpecObjectPtr { .. } | CheckedProjection::SpecObjectVtable => {
                Err(self.err(span, CompErrorKind::Unsupported("accessing a spec object's pointer/vtable inside a 'comp' evaluation")))
            }
        }
    }

    fn write_place(&mut self, place: &CheckedPlace, value: ConstValue, span: Span) -> CompResult<()> {
        if place.projections.is_empty() {
            return self.write_root(&place.root, value, span);
        }
        // A projected write (`a.b = x`, `a[i] = x`) reads the whole root
        // value, mutates the projected-into leaf, and writes the whole
        // value back -- there's no real memory here to mutate in place
        // through, only a `ConstValue` tree, so "mutate a leaf" is
        // structurally a "rebuild the tree" operation, same as everywhere
        // else in this interpreter.
        let root_value = self.read_root(&place.root, span)?;
        let updated = self.write_projections(root_value, &place.projections, value, span)?;
        self.write_root(&place.root, updated, span)
    }

    fn write_projections(
        &mut self,
        base: ConstValue,
        projections: &[CheckedProjection],
        value: ConstValue,
        span: Span,
    ) -> CompResult<ConstValue> {
        let Some((first, rest)) = projections.split_first() else {
            return Ok(value);
        };
        match first {
            CheckedProjection::FieldAccess { index, .. } => match base {
                ConstValue::Struct(mut fields) => {
                    let inner = std::mem::replace(&mut fields[*index], ConstValue::Bool(false));
                    fields[*index] = self.write_projections(inner, rest, value, span)?;
                    Ok(ConstValue::Struct(fields))
                }
                _ => Err(self.err(span, CompErrorKind::Unsupported("field write on a non-struct comp value"))),
            },
            CheckedProjection::Index { index_expr, .. } => {
                let index = self.expect_index(index_expr)?;
                match base {
                    ConstValue::Array(mut v) => {
                        if index >= v.len() {
                            return Err(self.err(span, CompErrorKind::Unsupported("an out-of-range comp index write")));
                        }
                        let inner = std::mem::replace(&mut v[index], ConstValue::Bool(false));
                        v[index] = self.write_projections(inner, rest, value, span)?;
                        Ok(ConstValue::Array(v))
                    }
                    _ => Err(self.err(span, CompErrorKind::Unsupported("index write on a non-array comp value"))),
                }
            }
            CheckedProjection::UnionField { index, .. } => {
                let inner = self.write_projections(ConstValue::Bool(false), rest, value, span)?;
                Ok(ConstValue::Union { field_index: *index, value: Box::new(inner) })
            }
            CheckedProjection::Deref { .. } => Err(self.err(span, CompErrorKind::UnresolvableMemory)),
            CheckedProjection::SpecObjectPtr { .. } | CheckedProjection::SpecObjectVtable => Err(self.err(
                span,
                CompErrorKind::Unsupported("writing through a spec object's pointer/vtable inside a 'comp' evaluation"),
            )),
            _ => Err(self.err(span, CompErrorKind::Unsupported("this write target inside a comp evaluation"))),
        }
    }

    fn write_root(&mut self, root: &CheckedPlaceRoot, value: ConstValue, span: Span) -> CompResult<()> {
        match root {
            CheckedPlaceRoot::Variable { decl_id, storage: Storage::Local | Storage::Parameter, .. } => {
                self.frame().locals.insert(*decl_id, value);
                Ok(())
            }
            CheckedPlaceRoot::Variable { storage: Storage::Global, .. } => {
                Err(self.err(span, CompErrorKind::NonCompGlobalRead))
            }
            _ => Err(self.err(span, CompErrorKind::Unsupported("this assignment target inside a comp evaluation"))),
        }
    }
}

enum BlockResult {
    Value(ConstValue),
    /// The block ended with no tail expression -- either an ordinary
    /// `Void` block, or one that would only ever be reached with a value
    /// through a path analysis already proved unreachable. Callers treat
    /// this as `Void`, matching `CheckedBlock::tail`'s own convention.
    Diverged,
}

fn compare(op: BinaryOp, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        BinaryOp::Eq => ord == Equal,
        BinaryOp::Ne => ord != Equal,
        BinaryOp::Lt => ord == Less,
        BinaryOp::Le => ord != Greater,
        BinaryOp::Gt => ord == Greater,
        BinaryOp::Ge => ord != Less,
        _ => unreachable!("compare is only ever called for a comparison operator"),
    }
}

/// A cast's numeric conversion, applied directly to `NumberValue`'s own
/// wide (`i64`/`u64`/`f64`) representation -- truncates/masks to the
/// target's declared width exactly like Cranelift's `ireduce`/`sextend`/
/// `uextend` do, matching ordinary (non-`comp`) cast codegen's own
/// wraparound semantics rather than erroring out of range (unlike a
/// literal's range check in `Analyzer::const_number`, a cast is defined to
/// wrap, not to reject).
fn cast_number(n: NumberValue, target: crate::resolved_type::NumericKind) -> NumberValue {
    use crate::resolved_type::NumericKind;
    let mask = |width: u32, bits: u64| if width >= 64 { bits } else { bits & ((1u64 << width) - 1) };
    let sign_extend = |width: u32, bits: u64| -> i64 {
        if width >= 64 {
            bits as i64
        } else {
            let shift = 64 - width;
            ((bits << shift) as i64) >> shift
        }
    };
    let raw_bits = match n {
        NumberValue::Signed(v) => v as u64,
        NumberValue::Unsigned(v) => v,
        NumberValue::Float(f) => f as i64 as u64,
    };
    match target {
        NumericKind::Signed(width) => match n {
            NumberValue::Float(f) => NumberValue::Signed(f as i64),
            _ => NumberValue::Signed(sign_extend(width, mask(width, raw_bits))),
        },
        NumericKind::Unsigned(width) => match n {
            NumberValue::Float(f) => NumberValue::Unsigned(f as u64),
            _ => NumberValue::Unsigned(mask(width, raw_bits)),
        },
        NumericKind::Float(width) => {
            let as_f64 = match n {
                NumberValue::Signed(v) => v as f64,
                NumberValue::Unsigned(v) => v as f64,
                NumberValue::Float(f) => f,
            };
            NumberValue::Float(if width == 32 { as_f64 as f32 as f64 } else { as_f64 })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checked::{
        CheckedAssignment, CheckedIf, CheckedParam, CheckedStructLiteral, CheckedStructLiteralField, CheckedWhile,
    };
    use omega_hir::ModuleId;

    fn id(n: u32) -> HirId {
        HirId { module: ModuleId(0), local: n }
    }

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn node(kind: CheckedExpr, r#type: ResolvedType) -> CheckedExprNode {
        CheckedExprNode { id: id(9999), span: sp(), r#type, kind }
    }

    fn num(n: i64) -> CheckedExprNode {
        node(CheckedExpr::Number(NumberValue::Signed(n)), ResolvedType::I32)
    }

    fn local_place(decl: HirId, r#type: ResolvedType) -> CheckedPlace {
        CheckedPlace { root: CheckedPlaceRoot::Variable { decl_id: decl, storage: Storage::Local, r#type }, projections: vec![] }
    }

    struct NoFunctions;
    impl CompFunctionResolver for NoFunctions {
        fn resolve_function_body(&mut self, _decl_id: HirId) -> Result<Option<CheckedFunctionDef>, ResolveError> {
            panic!("this test never calls a function")
        }
    }

    #[test]
    fn arithmetic_folds() {
        let expr = node(
            CheckedExpr::BinaryOp(CheckedBinaryOp { op: BinaryOp::Add, left: Box::new(num(10)), right: Box::new(num(20)) }),
            ResolvedType::I32,
        );
        let value = eval(&mut NoFunctions, &expr).unwrap();
        assert_eq!(value, ConstValue::Number(NumberValue::Signed(30)));
    }

    #[test]
    fn division_by_zero_is_rejected_not_a_panic() {
        let expr = node(
            CheckedExpr::BinaryOp(CheckedBinaryOp { op: BinaryOp::Div, left: Box::new(num(1)), right: Box::new(num(0)) }),
            ResolvedType::I32,
        );
        let err = eval(&mut NoFunctions, &expr).unwrap_err();
        assert!(matches!(err.kind, CompErrorKind::Unsupported(_)));
    }

    #[test]
    fn if_else_picks_the_taken_branch() {
        let cond = node(CheckedExpr::Bool(false), ResolvedType::Bool);
        let then_block = CheckedBlock { stmts: vec![], tail: Some(Box::new(num(1))) };
        let else_block = CheckedBlock { stmts: vec![], tail: Some(Box::new(num(2))) };
        let expr = node(
            CheckedExpr::If(CheckedIf { branches: vec![(cond, then_block)], else_branch: Some(else_block) }),
            ResolvedType::I32,
        );
        let value = eval(&mut NoFunctions, &expr).unwrap();
        assert_eq!(value, ConstValue::Number(NumberValue::Signed(2)));
    }

    #[test]
    fn struct_literal_builds_fields_in_declared_order() {
        let struct_ty = ResolvedType::Bool; // placeholder -- struct fields don't need a real ResolvedStructType for this test
        let lit = CheckedStructLiteral {
            fields: vec![
                CheckedStructLiteralField { field_index: 1, value: num(20) },
                CheckedStructLiteralField { field_index: 0, value: num(10) },
            ],
        };
        let expr = node(CheckedExpr::StructLiteral(lit), struct_ty);
        let value = eval(&mut NoFunctions, &expr).unwrap();
        assert_eq!(
            value,
            ConstValue::Struct(vec![ConstValue::Number(NumberValue::Signed(10)), ConstValue::Number(NumberValue::Signed(20))])
        );
    }

    #[test]
    fn while_loop_accumulates_via_locals() {
        // comp-equivalent of: mut i := 0; mut sum := 0; while i < 5 { sum = sum + i; i = i + 1; } sum
        let i_id = id(1);
        let sum_id = id(2);
        let i_place = local_place(i_id, ResolvedType::I32);
        let sum_place = local_place(sum_id, ResolvedType::I32);

        let cond = node(
            CheckedExpr::BinaryOp(CheckedBinaryOp {
                op: BinaryOp::Lt,
                left: Box::new(node(CheckedExpr::Place(i_place.clone()), ResolvedType::I32)),
                right: Box::new(num(5)),
            }),
            ResolvedType::Bool,
        );
        let sum_incr = CheckedStmt::Expression(node(
            CheckedExpr::Assignment(CheckedAssignment {
                target: sum_place.clone(),
                value: Box::new(node(
                    CheckedExpr::BinaryOp(CheckedBinaryOp {
                        op: BinaryOp::Add,
                        left: Box::new(node(CheckedExpr::Place(sum_place.clone()), ResolvedType::I32)),
                        right: Box::new(node(CheckedExpr::Place(i_place.clone()), ResolvedType::I32)),
                    }),
                    ResolvedType::I32,
                )),
            }),
            ResolvedType::I32,
        ));
        let i_incr = CheckedStmt::Expression(node(
            CheckedExpr::Assignment(CheckedAssignment {
                target: i_place.clone(),
                value: Box::new(node(
                    CheckedExpr::BinaryOp(CheckedBinaryOp {
                        op: BinaryOp::Add,
                        left: Box::new(node(CheckedExpr::Place(i_place.clone()), ResolvedType::I32)),
                        right: Box::new(num(1)),
                    }),
                    ResolvedType::I32,
                )),
            }),
            ResolvedType::I32,
        ));
        let body = CheckedBlock { stmts: vec![sum_incr, i_incr], tail: None };
        let while_stmt = CheckedStmt::While(CheckedWhile { id: id(3), span: sp(), condition: cond, body });

        let init_i = CheckedStmt::Expression(node(
            CheckedExpr::Assignment(CheckedAssignment { target: i_place.clone(), value: Box::new(num(0)) }),
            ResolvedType::I32,
        ));
        let init_sum = CheckedStmt::Expression(node(
            CheckedExpr::Assignment(CheckedAssignment { target: sum_place.clone(), value: Box::new(num(0)) }),
            ResolvedType::I32,
        ));

        let outer = CheckedBlock {
            stmts: vec![init_i, init_sum, while_stmt],
            tail: Some(Box::new(node(CheckedExpr::Place(sum_place), ResolvedType::I32))),
        };
        let expr = node(CheckedExpr::Codeblock(outer), ResolvedType::I32);

        let value = eval(&mut NoFunctions, &expr).unwrap();
        assert_eq!(value, ConstValue::Number(NumberValue::Signed(0 + 1 + 2 + 3 + 4)));
    }

    #[test]
    fn infinite_loop_exhausts_fuel_instead_of_hanging() {
        let cond = node(CheckedExpr::Bool(true), ResolvedType::Bool);
        let body = CheckedBlock { stmts: vec![], tail: None };
        let while_stmt = CheckedStmt::While(CheckedWhile { id: id(1), span: sp(), condition: cond, body });
        let outer = CheckedBlock { stmts: vec![while_stmt], tail: Some(Box::new(num(0))) };
        let expr = node(CheckedExpr::Codeblock(outer), ResolvedType::I32);

        let err = eval(&mut NoFunctions, &expr).unwrap_err();
        assert!(matches!(err.kind, CompErrorKind::FuelExhausted));
    }

    #[test]
    fn calling_an_extern_is_rejected_with_a_precise_reason() {
        struct AllExtern;
        impl CompFunctionResolver for AllExtern {
            fn resolve_function_body(&mut self, _decl_id: HirId) -> Result<Option<CheckedFunctionDef>, ResolveError> {
                Ok(None)
            }
        }
        let callee = node(
            CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable {
                    decl_id: id(42),
                    storage: Storage::Function,
                    r#type: ResolvedType::Function(crate::resolved_type::ResolvedFunctionType {
                        params: vec![],
                        return_type: Box::new(ResolvedType::Void),
                        is_variadic: false,
                        self_mode: None,
                    }),
                },
                projections: vec![],
            }),
            ResolvedType::Function(crate::resolved_type::ResolvedFunctionType {
                params: vec![],
                return_type: Box::new(ResolvedType::Void),
                is_variadic: false,
                self_mode: None,
            }),
        );
        let call = node(
            CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(callee),
                fn_type: crate::resolved_type::ResolvedFunctionType {
                    params: vec![],
                    return_type: Box::new(ResolvedType::Void),
                    is_variadic: false,
                    self_mode: None,
                },
                args: vec![],
            }),
            ResolvedType::Void,
        );

        let err = eval(&mut AllExtern, &call).unwrap_err();
        assert!(matches!(err.kind, CompErrorKind::ExternCall));
    }

    #[test]
    fn calling_another_function_interprets_its_own_body() {
        // add(a: i32, b: i32) => i32 { a + b } ; comp add(10, 20)
        let a_id = id(1);
        let b_id = id(2);
        let add_body = CheckedBlock {
            stmts: vec![],
            tail: Some(Box::new(node(
                CheckedExpr::BinaryOp(CheckedBinaryOp {
                    op: BinaryOp::Add,
                    left: Box::new(node(CheckedExpr::Place(local_place(a_id, ResolvedType::I32)), ResolvedType::I32)),
                    right: Box::new(node(CheckedExpr::Place(local_place(b_id, ResolvedType::I32)), ResolvedType::I32)),
                }),
                ResolvedType::I32,
            ))),
        };
        let add_def = CheckedFunctionDef {
            id: id(100),
            span: sp(),
            name: omega_parser::prelude::Ident("add".into()),
            type_args: vec![],
            self_mode: None,
            is_variadic: false,
            params: vec![
                CheckedParam { id: a_id, span: sp(), ident: omega_parser::prelude::Ident("a".into()), r#type: ResolvedType::I32 },
                CheckedParam { id: b_id, span: sp(), ident: omega_parser::prelude::Ident("b".into()), r#type: ResolvedType::I32 },
            ],
            return_type: ResolvedType::I32,
            body: add_body,
            inline: None,
            mangling: crate::annotations::ManglingMode::Enabled,
            extension_target: None,
        };

        struct OneFunction(CheckedFunctionDef);
        impl CompFunctionResolver for OneFunction {
            fn resolve_function_body(&mut self, decl_id: HirId) -> Result<Option<CheckedFunctionDef>, ResolveError> {
                if decl_id == self.0.id { Ok(Some(self.0.clone())) } else { Ok(None) }
            }
        }

        let fn_type = crate::resolved_type::ResolvedFunctionType {
            params: vec![(omega_parser::prelude::Ident("a".into()), ResolvedType::I32), (omega_parser::prelude::Ident("b".into()), ResolvedType::I32)],
            return_type: Box::new(ResolvedType::I32),
            is_variadic: false,
            self_mode: None,
        };
        let callee = node(
            CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id: id(100), storage: Storage::Function, r#type: ResolvedType::Function(fn_type.clone()) },
                projections: vec![],
            }),
            ResolvedType::Function(fn_type.clone()),
        );
        let call = node(
            CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee), fn_type, args: vec![num(10), num(20)] }),
            ResolvedType::I32,
        );

        let mut resolver = OneFunction(add_def);
        let value = eval(&mut resolver, &call).unwrap();
        assert_eq!(value, ConstValue::Number(NumberValue::Signed(30)));
    }
}
