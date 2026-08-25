use crate::checked::{
    CastKind, CheckedAnonymousEnumWiden, CheckedBinaryOp, CheckedBlock, CheckedCoercion,
    CheckedCoercionStep, CheckedExpr, CheckedExprNode, CheckedFor, CheckedFunctionCall,
    CheckedFunctionDef, CheckedIf, CheckedLoop, CheckedMatch, CheckedPlace, CheckedPlaceRoot,
    CheckedProjection, CheckedStmt, CheckedTry, CheckedWhile, NumberValue, Storage,
};
use crate::resolved_type::{ConstValue, ResolvedType};
use crate::resolver::ResolveError;
use crate::target::Target;
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Span};
use std::collections::HashMap;

const FUEL_LIMIT: u32 = 1_000_000;

pub trait CompFunctionResolver {
    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<CheckedFunctionDef>, ResolveError>;
}

impl CompFunctionResolver for dyn crate::resolver::ModuleResolver + '_ {
    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
        crate::resolver::ModuleResolver::resolve_function_body(self, decl_id)
    }
}

#[derive(Debug, Clone)]
pub enum CompErrorKind {
    ExternCall,
    DynamicDispatch,
    UnresolvableMemory,
    NonCompGlobalRead,
    ReadBeforeInit,
    ResolutionFailed(ResolveError),
    FuelExhausted,
    EscapingControlFlow,
    Unsupported(&'static str),
}

impl std::fmt::Display for CompErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExternCall => write!(f, "it calls an 'extern' function"),
            Self::DynamicDispatch => write!(f, "it uses dynamic dispatch through a 'spec' object"),
            Self::UnresolvableMemory => write!(
                f,
                "it dereferences a pointer this evaluation didn't itself produce"
            ),
            Self::NonCompGlobalRead => {
                write!(f, "it reads a global that isn't itself a 'comp' binding")
            }
            Self::ReadBeforeInit => write!(f, "it reads a local before it's ever assigned"),
            Self::ResolutionFailed(e) => write!(f, "{e}"),
            Self::FuelExhausted => write!(f, "it ran for too long (a runaway loop or recursion)"),
            Self::EscapingControlFlow => write!(
                f,
                "it exits the enclosing function, which a compile-time evaluation cannot do"
            ),
            Self::Unsupported(what) => {
                write!(f, "{what} isn't supported in a compile-time evaluation yet")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompError {
    pub kind: CompErrorKind,
    pub span: Span,
    pub trace: Vec<Span>,
}

enum Signal {
    Return(ConstValue),
    Break,
    Continue,
}

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

#[derive(Default)]
struct Frame {
    locals: HashMap<HirId, ConstValue>,
    defers: Vec<CheckedBlock>,
}

struct Interpreter<'r, R: CompFunctionResolver + ?Sized> {
    resolver: &'r mut R,
    target: Target,
    fuel: u32,
    frames: Vec<Frame>,
    call_trace: Vec<Span>,
}

pub fn eval<R: CompFunctionResolver + ?Sized>(
    resolver: &mut R,
    expr: &CheckedExprNode,
    target: Target,
) -> Result<ConstValue, CompError> {
    let mut interp = Interpreter {
        resolver,
        target,
        fuel: FUEL_LIMIT,
        frames: vec![Frame::default()],
        call_trace: vec![],
    };
    let result = interp.eval_expr(expr);
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
        // Reachable when the evaluated expression itself exits the enclosing
        // function, e.g. a `comp` operand whose `?` fails: the signal has no
        // frame left to unwind into.
        Err(Outcome::Signal(_)) => Err(CompError {
            kind: CompErrorKind::EscapingControlFlow,
            span: expr.span,
            trace: Vec::new(),
        }),
    }
}

impl<'r, R: CompFunctionResolver + ?Sized> Interpreter<'r, R> {
    fn err(&self, span: Span, kind: CompErrorKind) -> Outcome {
        Outcome::Error(CompError {
            kind,
            span,
            trace: self.call_trace.clone(),
        })
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .expect("comp evaluation always has at least one frame")
    }

    fn tick(&mut self, span: Span) -> CompResult<()> {
        if self.fuel == 0 {
            return Err(self.err(span, CompErrorKind::FuelExhausted));
        }
        self.fuel -= 1;
        Ok(())
    }

    fn eval_expr(&mut self, node: &CheckedExprNode) -> CompResult<ConstValue> {
        self.tick(node.span)?;
        match &node.kind {
            CheckedExpr::Place(place) => self.read_place(place, node.span),
            CheckedExpr::Number(n) => Ok(ConstValue::Number(*n)),
            CheckedExpr::Bool(b) => Ok(ConstValue::Bool(*b)),
            CheckedExpr::Char(c) => Ok(ConstValue::Char(*c)),
            CheckedExpr::String(s) => Ok(ConstValue::Str(s.clone())),
            CheckedExpr::ByteString(s) => Ok(ConstValue::Slice(
                s.bytes()
                    .map(|b| ConstValue::Number(NumberValue::Unsigned(b as u64)))
                    .collect(),
            )),
            CheckedExpr::Const(value) => Ok(value.clone()),
            CheckedExpr::FunctionCall(call) => self.eval_call(call, node.span),
            CheckedExpr::Assignment(assign) => {
                let value = self.eval_expr(&assign.value)?;
                self.write_place(&assign.target, value.clone(), node.span)?;
                Ok(value)
            }
            CheckedExpr::CompoundAssign(compound) => {
                let read = self.read_place(&compound.place, node.span)?;
                let read = match &compound.read_cast {
                    Some((kind, target_type)) => {
                        self.apply_cast(*kind, target_type, read, node.span)?
                    }
                    None => read,
                };
                let rhs = self.eval_expr(&compound.value)?;
                let result = match (read, rhs) {
                    (ConstValue::Number(l), ConstValue::Number(r)) => {
                        self.eval_numeric_binary_op(compound.op, l, r, node.span)?
                    }
                    (ConstValue::Bool(l), ConstValue::Bool(r)) => {
                        self.eval_bool_binary_op(compound.op, l, r, node.span)?
                    }
                    (ConstValue::Char(l), ConstValue::Char(r)) => {
                        self.eval_char_binary_op(compound.op, l, r, node.span)?
                    }
                    _ => {
                        return Err(self.err(
                            node.span,
                            CompErrorKind::Unsupported(
                                "compound-assign operator on this comp value shape",
                            ),
                        ));
                    }
                };
                self.write_place(&compound.place, result.clone(), node.span)?;
                Ok(result)
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
                let mut values: Vec<Option<ConstValue>> =
                    (0..lit.fields.len()).map(|_| None).collect();
                for field in &lit.fields {
                    let value = self.eval_expr(&field.value)?;
                    if field.field_index >= values.len() {
                        values.resize(field.field_index + 1, None);
                    }
                    values[field.field_index] = Some(value);
                }
                let fields = values
                    .into_iter()
                    .map(|v| {
                        v.expect(
                            "analysis guarantees every declared field is initialized exactly once",
                        )
                    })
                    .collect();
                Ok(ConstValue::Struct(fields))
            }
            CheckedExpr::EnumConstruct(construct) => {
                let (tag, header, dynamic_count) =
                    self.enum_variant_facts(&node.r#type, construct.variant_index)?;
                let mut values: Vec<Option<ConstValue>> =
                    (0..construct.fields.len()).map(|_| None).collect();
                for field in &construct.fields {
                    let value = self.eval_expr(&field.value)?;
                    if field.field_index >= values.len() {
                        values.resize(field.field_index + 1, None);
                    }
                    values[field.field_index] = Some(value);
                }
                let mut values: Vec<ConstValue> = values
                    .into_iter()
                    .map(|v| {
                        v.expect(
                            "analysis guarantees every declared field is initialized exactly once",
                        )
                    })
                    .collect();
                let fields = values.split_off(dynamic_count);
                Ok(ConstValue::Enum {
                    variant_index: construct.variant_index,
                    tag,
                    header,
                    dynamic_fields: values,
                    fields,
                })
            }
            CheckedExpr::UnionConstruct(construct) => {
                let value = self.eval_expr(&construct.value)?;
                Ok(ConstValue::Union {
                    field_index: construct.field_index,
                    value: Box::new(value),
                })
            }
            CheckedExpr::Slice(slice) => self.eval_slice(slice, node.span),
            CheckedExpr::Cast(cast) => self.eval_cast(cast, node.span),
            CheckedExpr::Sizeof(target) => Ok(ConstValue::Number(NumberValue::Unsigned(
                crate::layout::total_bytes(target, self.target.pointer_bytes()) as u64,
            ))),
            CheckedExpr::AnonymousEnumWiden(widen) => self.eval_anonymous_enum_widen(node, widen),
            CheckedExpr::Try(r#try) => self.eval_try(r#try, node.span),
            CheckedExpr::SpecCoerce(_) => Err(self.err(node.span, CompErrorKind::DynamicDispatch)),
            CheckedExpr::DynamicCall(_) => Err(self.err(node.span, CompErrorKind::DynamicDispatch)),
        }
    }

    /// Retags a constant anonymous enum under the wider shape. The payload is
    /// carried over untouched; only the variant index and tag are remapped.
    fn eval_anonymous_enum_widen(
        &mut self,
        node: &CheckedExprNode,
        widen: &CheckedAnonymousEnumWiden,
    ) -> CompResult<ConstValue> {
        let ConstValue::Enum {
            variant_index,
            fields,
            ..
        } = self.eval_expr(&widen.source)?
        else {
            unreachable!("analysis guarantees an anonymous-enum source for widening")
        };
        let target_index = widen.variant_map[variant_index];
        let (tag, header, _) = self.enum_variant_facts(&node.r#type, target_index)?;
        Ok(ConstValue::Enum {
            variant_index: target_index,
            tag,
            header,
            dynamic_fields: Vec::new(),
            fields,
        })
    }

    /// Evaluates the operand once, then either yields its success payload or
    /// builds the enclosing function's failure value and returns it through
    /// the ordinary return signal, so registered defers still run.
    fn eval_try(&mut self, r#try: &CheckedTry, span: Span) -> CompResult<ConstValue> {
        let ConstValue::Enum {
            variant_index,
            fields,
            ..
        } = self.eval_expr(&r#try.operand)?
        else {
            unreachable!("analysis guarantees a canonical fallible enum operand")
        };

        if variant_index == r#try.source.success_variant {
            return Ok(fields
                .into_iter()
                .nth(r#try.source.success_field)
                .expect("analysis resolved the success payload's field index"));
        }

        let destination = &r#try.destination;
        let mut slots: Vec<Option<ConstValue>> = Vec::new();
        if let (Some((source_field, _)), Some(field_index)) =
            (&r#try.source.failure_payload, destination.failure_field)
        {
            let error = fields
                .into_iter()
                .nth(*source_field)
                .expect("analysis resolved the failure payload's field index");
            let error = self.eval_coercion(&destination.error_coercion, error, span)?;
            slots.resize(field_index + 1, None);
            slots[field_index] = Some(error);
        }
        let fields: Vec<ConstValue> = slots
            .into_iter()
            .map(|slot| slot.expect("a canonical failure variant declares one payload field"))
            .collect();

        let (tag, header, _) =
            self.enum_variant_facts(&destination.r#type, destination.failure_variant)?;
        Err(Outcome::Signal(Signal::Return(ConstValue::Enum {
            variant_index: destination.failure_variant,
            tag,
            header,
            dynamic_fields: Vec::new(),
            fields,
        })))
    }

    /// Replays a coercion the analyzer already decided is legal. No step here
    /// re-asks whether the conversion is allowed.
    fn eval_coercion(
        &mut self,
        plan: &CheckedCoercion,
        value: ConstValue,
        span: Span,
    ) -> CompResult<ConstValue> {
        let mut value = value;
        for step in &plan.steps {
            value = match step {
                CheckedCoercionStep::ProjectAnonymousMember { .. } => {
                    let ConstValue::Enum { fields, .. } = value else {
                        unreachable!("analysis guarantees a refined anonymous-enum source")
                    };
                    fields
                        .into_iter()
                        .next()
                        .expect("an anonymous-enum member is its variant's only field")
                }
                CheckedCoercionStep::InjectAnonymousMember { variant_index, .. } => {
                    ConstValue::anonymous_enum(*variant_index, vec![value])
                }
                CheckedCoercionStep::WidenAnonymousEnum { variant_map, .. } => {
                    let ConstValue::Enum {
                        variant_index,
                        fields,
                        ..
                    } = value
                    else {
                        unreachable!("analysis guarantees an anonymous-enum widening source")
                    };
                    ConstValue::anonymous_enum(variant_map[variant_index], fields)
                }
                CheckedCoercionStep::SpecCoerce { .. } => {
                    return Err(self.err(span, CompErrorKind::DynamicDispatch));
                }
            };
        }
        Ok(value)
    }

    fn enum_variant_facts(
        &self,
        r#type: &ResolvedType,
        variant_index: usize,
    ) -> CompResult<(NumberValue, Vec<ConstValue>, usize)> {
        let view = crate::layout::EnumView::of(r#type)
            .expect("an enum construction's own type is always an enum-like type");
        let variant = &view.variants[variant_index];
        Ok((
            variant.tag,
            variant.header_values.clone(),
            view.dynamic_fields.len(),
        ))
    }

    fn eval_negate(&mut self, inner: &CheckedExprNode, span: Span) -> CompResult<ConstValue> {
        match self.eval_expr(inner)? {
            ConstValue::Number(NumberValue::Signed(n)) => {
                Ok(ConstValue::Number(NumberValue::Signed(n.wrapping_neg())))
            }
            ConstValue::Number(NumberValue::Float(f)) => {
                Ok(ConstValue::Number(NumberValue::Float(-f)))
            }
            _ => Err(self.err(
                span,
                CompErrorKind::Unsupported("negation of a non-numeric comp value"),
            )),
        }
    }

    fn eval_bitnot(&mut self, inner: &CheckedExprNode, span: Span) -> CompResult<ConstValue> {
        match self.eval_expr(inner)? {
            ConstValue::Number(NumberValue::Signed(n)) => {
                Ok(ConstValue::Number(NumberValue::Signed(!n)))
            }
            ConstValue::Number(NumberValue::Unsigned(n)) => {
                Ok(ConstValue::Number(NumberValue::Unsigned(!n)))
            }
            ConstValue::Bool(b) => Ok(ConstValue::Bool(!b)),
            _ => Err(self.err(
                span,
                CompErrorKind::Unsupported("bitwise-not of a non-integer comp value"),
            )),
        }
    }

    fn eval_binary_op(&mut self, bin: &CheckedBinaryOp, span: Span) -> CompResult<ConstValue> {
        let left = self.eval_expr(&bin.left)?;
        let right = self.eval_expr(&bin.right)?;
        match (left, right) {
            (ConstValue::Number(l), ConstValue::Number(r)) => {
                self.eval_numeric_binary_op(bin.op, l, r, span)
            }
            (ConstValue::Bool(l), ConstValue::Bool(r)) => {
                self.eval_bool_binary_op(bin.op, l, r, span)
            }
            (ConstValue::Char(l), ConstValue::Char(r)) => {
                self.eval_char_binary_op(bin.op, l, r, span)
            }
            _ => Err(self.err(
                span,
                CompErrorKind::Unsupported("binary operator on this comp value shape"),
            )),
        }
    }

    fn eval_bool_binary_op(
        &mut self,
        op: BinaryOp,
        l: bool,
        r: bool,
        span: Span,
    ) -> CompResult<ConstValue> {
        match op {
            BinaryOp::Eq => Ok(ConstValue::Bool(l == r)),
            BinaryOp::Ne => Ok(ConstValue::Bool(l != r)),
            BinaryOp::BitAnd => Ok(ConstValue::Bool(l & r)),
            BinaryOp::BitOr => Ok(ConstValue::Bool(l | r)),
            BinaryOp::BitXor => Ok(ConstValue::Bool(l ^ r)),
            _ => Err(self.err(span, CompErrorKind::Unsupported("this operator on bool"))),
        }
    }

    fn eval_char_binary_op(
        &mut self,
        op: BinaryOp,
        l: char,
        r: char,
        span: Span,
    ) -> CompResult<ConstValue> {
        if op.is_comparison() {
            let ord = (l as u32).cmp(&(r as u32));
            return Ok(ConstValue::Bool(compare(op, ord)));
        }
        Err(self.err(span, CompErrorKind::Unsupported("arithmetic on char")))
    }

    fn eval_numeric_binary_op(
        &mut self,
        op: BinaryOp,
        l: NumberValue,
        r: NumberValue,
        span: Span,
    ) -> CompResult<ConstValue> {
        use NumberValue::*;
        if op.is_comparison() {
            let ord = match (l, r) {
                (Signed(l), Signed(r)) => l.cmp(&r),
                (Unsigned(l), Unsigned(r)) => l.cmp(&r),
                (Float(l), Float(r)) => match l.partial_cmp(&r) {
                    Some(ord) => ord,
                    None => return Ok(ConstValue::Bool(false)),
                },
                _ => {
                    return Err(self.err(
                        span,
                        CompErrorKind::Unsupported("comparison across numeric kinds"),
                    ));
                }
            };
            if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
                && let (Float(l), Float(r)) = (l, r)
            {
                return Ok(ConstValue::Bool(if op == BinaryOp::Eq {
                    l == r
                } else {
                    l != r
                }));
            }
            return Ok(ConstValue::Bool(compare(op, ord)));
        }
        match (l, r) {
            (Signed(l), Signed(r)) => self.eval_signed_arith(op, l, r, span),
            (Unsigned(l), Unsigned(r)) => self.eval_unsigned_arith(op, l, r, span),
            (Float(l), Float(r)) => self.eval_float_arith(op, l, r, span),
            _ => Err(self.err(
                span,
                CompErrorKind::Unsupported("arithmetic across numeric kinds"),
            )),
        }
    }

    fn eval_signed_arith(
        &mut self,
        op: BinaryOp,
        l: i64,
        r: i64,
        span: Span,
    ) -> CompResult<ConstValue> {
        let v = match op {
            BinaryOp::Add => l.wrapping_add(r),
            BinaryOp::Sub => l.wrapping_sub(r),
            BinaryOp::Mul => l.wrapping_mul(r),
            BinaryOp::Div if r == 0 => {
                return Err(self.err(span, CompErrorKind::Unsupported("division by zero")));
            }
            BinaryOp::Div => l.wrapping_div(r),
            BinaryOp::Rem if r == 0 => {
                return Err(self.err(span, CompErrorKind::Unsupported("division by zero")));
            }
            BinaryOp::Rem => l.wrapping_rem(r),
            BinaryOp::BitAnd => l & r,
            BinaryOp::BitOr => l | r,
            BinaryOp::BitXor => l ^ r,
            BinaryOp::Shl => l.wrapping_shl(r as u32),
            BinaryOp::Shr => l.wrapping_shr(r as u32),
            _ => {
                return Err(self.err(
                    span,
                    CompErrorKind::Unsupported("this operator on a signed integer"),
                ));
            }
        };
        Ok(ConstValue::Number(NumberValue::Signed(v)))
    }

    fn eval_unsigned_arith(
        &mut self,
        op: BinaryOp,
        l: u64,
        r: u64,
        span: Span,
    ) -> CompResult<ConstValue> {
        let v = match op {
            BinaryOp::Add => l.wrapping_add(r),
            BinaryOp::Sub => l.wrapping_sub(r),
            BinaryOp::Mul => l.wrapping_mul(r),
            BinaryOp::Div if r == 0 => {
                return Err(self.err(span, CompErrorKind::Unsupported("division by zero")));
            }
            BinaryOp::Div => l.wrapping_div(r),
            BinaryOp::Rem if r == 0 => {
                return Err(self.err(span, CompErrorKind::Unsupported("division by zero")));
            }
            BinaryOp::Rem => l.wrapping_rem(r),
            BinaryOp::BitAnd => l & r,
            BinaryOp::BitOr => l | r,
            BinaryOp::BitXor => l ^ r,
            BinaryOp::Shl => l.wrapping_shl(r as u32),
            BinaryOp::Shr => l.wrapping_shr(r as u32),
            _ => {
                return Err(self.err(
                    span,
                    CompErrorKind::Unsupported("this operator on an unsigned integer"),
                ));
            }
        };
        Ok(ConstValue::Number(NumberValue::Unsigned(v)))
    }

    fn eval_float_arith(
        &mut self,
        op: BinaryOp,
        l: f64,
        r: f64,
        span: Span,
    ) -> CompResult<ConstValue> {
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

    fn eval_cast(
        &mut self,
        cast: &crate::checked::CheckedCast,
        span: Span,
    ) -> CompResult<ConstValue> {
        let base = self.eval_expr(&cast.base)?;
        self.apply_cast(cast.kind, &cast.target_type, base, span)
    }

    fn apply_cast(
        &mut self,
        kind: CastKind,
        target_type: &ResolvedType,
        base: ConstValue,
        span: Span,
    ) -> CompResult<ConstValue> {
        match kind {
            CastKind::Reinterpret => Ok(base),
            // The operand was already evaluated for its effects; `void` has no
            // comp value, so this reuses the interpreter's no-value sentinel.
            CastKind::Discard => Ok(ConstValue::Bool(false)),
            CastKind::DropLength => match base {
                ConstValue::Str(s) => {
                    let bytes = s
                        .bytes()
                        .map(|b| ConstValue::Number(NumberValue::Unsigned(b as u64)))
                        .collect();
                    Ok(ConstValue::Ref(Box::new(ConstValue::Array(bytes))))
                }
                ConstValue::Slice(elements) => {
                    Ok(ConstValue::Ref(Box::new(ConstValue::Array(elements))))
                }
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported(
                        "a fat-to-thin pointer cast of a non-str/slice comp value",
                    ),
                )),
            },
            CastKind::Unsize => Err(self.err(
                span,
                CompErrorKind::Unsupported("a sized-array-to-slice cast of a comp value"),
            )),
            // Unreachable in practice (a vtable has no comp-time meaning),
            // kept explicit rather than folded into the numeric catch-all
            // below, which would misdescribe what was attempted.
            CastKind::SpecNarrow { .. } => Err(self.err(span, CompErrorKind::DynamicDispatch)),
            _ => {
                let ConstValue::Number(n) = base else {
                    return Err(self.err(
                        span,
                        CompErrorKind::Unsupported("a numeric cast of a non-numeric comp value"),
                    ));
                };
                let Some(target) = target_type.numeric_kind(self.target.pointer_bits()) else {
                    return Err(self.err(
                        span,
                        CompErrorKind::Unsupported("a cast to a non-numeric type"),
                    ));
                };
                Ok(ConstValue::Number(cast_number(n, target)))
            }
        }
    }

    fn eval_slice(
        &mut self,
        slice: &crate::checked::CheckedSlice,
        span: Span,
    ) -> CompResult<ConstValue> {
        let base = self.read_place(&slice.base, span)?;
        let elements = match base {
            ConstValue::Array(v) | ConstValue::Slice(v) => v,
            _ => {
                return Err(self.err(
                    span,
                    CompErrorKind::Unsupported("slicing a non-array/slice comp value"),
                ));
            }
        };
        let start = match &slice.start {
            Some(e) => self.expect_index(e)?,
            None => 0,
        };
        let end = match &slice.end {
            crate::checked::CheckedRangeEnd::Inclusive(end) => self.expect_index(end)? + 1,
            crate::checked::CheckedRangeEnd::Exclusive(end) => self.expect_index(end)?,
            crate::checked::CheckedRangeEnd::Open => elements.len(),
        };
        if start > end || end > elements.len() {
            return Err(self.err(
                span,
                CompErrorKind::Unsupported("an out-of-range comp slice"),
            ));
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
                _ => {
                    return Err(
                        self.err(span, CompErrorKind::Unsupported("a non-bool if-condition"))
                    );
                }
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
        for arm in &match_expr.arms {
            'groups: for group in &arm.conditions {
                for condition in group {
                    match self.eval_expr(condition)? {
                        ConstValue::Bool(true) => {}
                        ConstValue::Bool(false) => continue 'groups,
                        _ => {
                            return Err(self.err(
                                span,
                                CompErrorKind::Unsupported("a non-bool match condition"),
                            ));
                        }
                    }
                }
                return match self.eval_block(&arm.body)? {
                    BlockResult::Value(v) => Ok(v),
                    BlockResult::Diverged => Ok(ConstValue::Bool(false)),
                };
            }
        }
        match &match_expr.else_branch {
            Some(body) => match self.eval_block(body)? {
                BlockResult::Value(v) => Ok(v),
                BlockResult::Diverged => Ok(ConstValue::Bool(false)),
            },
            // Exhaustiveness was already proven by analysis; reaching here
            // means either a checked-tree invariant broke, or the
            // interpreter's own arm evaluation didn't reproduce analysis's
            // coverage proof (see docs/language/compile-time-evaluation.md).
            None => Err(self.err(
                span,
                CompErrorKind::Unsupported("an exhaustive match with no matching arm"),
            )),
        }
    }

    fn eval_call(&mut self, call: &CheckedFunctionCall, span: Span) -> CompResult<ConstValue> {
        let CheckedExpr::Place(CheckedPlace {
            root:
                CheckedPlaceRoot::Variable {
                    decl_id,
                    storage: Storage::Function,
                    ..
                },
            projections,
            r#type: _,
        }) = &call.callee.kind
        else {
            return Err(self.err(span, CompErrorKind::Unsupported("an indirect call")));
        };
        debug_assert!(
            projections.is_empty(),
            "a Storage::Function place is never itself projected"
        );

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
            Outcome::Signal(Signal::Return(_)) => {
                unreachable!("call_function always converts its own Return signal into a value")
            }
            other => other,
        })
    }

    fn call_function(
        &mut self,
        body: &CheckedFunctionDef,
        args: Vec<ConstValue>,
    ) -> CompResult<ConstValue> {
        let mut frame = Frame::default();
        for (param, value) in body.params.iter().zip(args) {
            frame.locals.insert(param.id, value);
        }
        self.frames.push(frame);
        let value = match self.eval_block(&body.body) {
            Ok(BlockResult::Value(v)) => Ok(v),
            Ok(BlockResult::Diverged) => Ok(ConstValue::Bool(false)),
            Err(Outcome::Signal(Signal::Return(v))) => Ok(v),
            Err(other) => Err(other),
        };
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
            CheckedStmt::Declaration(_) => Ok(()),
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
            CheckedStmt::Defer(d) => {
                self.frame().defers.push(d.body.clone());
                Ok(())
            }
            CheckedStmt::InlineAsm(asm) => {
                Err(self.err(asm.span, CompErrorKind::Unsupported("an 'asm' block")))
            }
        }
    }

    fn eval_while(&mut self, w: &CheckedWhile) -> CompResult<()> {
        loop {
            self.tick(w.span)?;
            match self.eval_expr(&w.condition)? {
                ConstValue::Bool(true) => {}
                ConstValue::Bool(false) => return Ok(()),
                _ => {
                    return Err(self.err(
                        w.span,
                        CompErrorKind::Unsupported("a non-bool while-condition"),
                    ));
                }
            }
            match self.eval_block(&w.body) {
                Ok(_) => {}
                Err(Outcome::Signal(Signal::Break)) => return Ok(()),
                Err(Outcome::Signal(Signal::Continue)) => continue,
                Err(other) => return Err(other),
            }
        }
    }

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
                _ => {
                    return Err(self.err(
                        f.span,
                        CompErrorKind::Unsupported("a non-bool for-condition"),
                    ));
                }
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

    fn read_place(&mut self, place: &CheckedPlace, span: Span) -> CompResult<ConstValue> {
        let mut value = self.read_root(&place.root, span)?;
        for proj in &place.projections {
            value = self.read_projection(value, proj, span)?;
        }
        Ok(value)
    }

    fn read_root(&mut self, root: &CheckedPlaceRoot, span: Span) -> CompResult<ConstValue> {
        match root {
            CheckedPlaceRoot::Variable {
                decl_id,
                storage: Storage::Local | Storage::Parameter,
                ..
            } => self
                .frame()
                .locals
                .get(decl_id)
                .cloned()
                .ok_or_else(|| self.err(span, CompErrorKind::ReadBeforeInit)),
            CheckedPlaceRoot::Variable {
                storage: Storage::Global,
                ..
            } => Err(self.err(span, CompErrorKind::NonCompGlobalRead)),
            CheckedPlaceRoot::Variable {
                storage: Storage::Function,
                ..
            } => Err(self.err(
                span,
                CompErrorKind::Unsupported("a function value used outside of a direct call"),
            )),
            CheckedPlaceRoot::Variable {
                storage: Storage::Comp,
                ..
            } => {
                unreachable!(
                    "a comp binding is substituted into CheckedExpr::Const during analysis -- see Storage::Comp's doc comment"
                )
            }
            CheckedPlaceRoot::Expr(expr) => self.eval_expr(expr),
        }
    }

    fn read_projection(
        &mut self,
        value: ConstValue,
        proj: &CheckedProjection,
        span: Span,
    ) -> CompResult<ConstValue> {
        match proj {
            CheckedProjection::FieldAccess { index, .. } => match value {
                ConstValue::Struct(fields) => Ok(fields[*index].clone()),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("field access on a non-struct comp value"),
                )),
            },
            CheckedProjection::Index { index_expr, .. } => {
                let index = self.expect_index(index_expr)?;
                match value {
                    ConstValue::Array(v) | ConstValue::Slice(v) => {
                        v.get(index).cloned().ok_or_else(|| {
                            self.err(
                                span,
                                CompErrorKind::Unsupported("an out-of-range comp index"),
                            )
                        })
                    }
                    _ => Err(self.err(
                        span,
                        CompErrorKind::Unsupported("indexing a non-array/slice comp value"),
                    )),
                }
            }
            CheckedProjection::Deref { .. } => match value {
                ConstValue::Ref(inner) => Ok(*inner),
                _ => Err(self.err(span, CompErrorKind::UnresolvableMemory)),
            },
            CheckedProjection::SliceLength => match value {
                ConstValue::Slice(v) | ConstValue::Array(v) => {
                    Ok(ConstValue::Number(NumberValue::Unsigned(v.len() as u64)))
                }
                ConstValue::Str(s) => Ok(ConstValue::Number(NumberValue::Unsigned(s.len() as u64))),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("length of a non-slice/str comp value"),
                )),
            },
            CheckedProjection::EnumTag { .. } => match value {
                ConstValue::Enum { tag, .. } => Ok(ConstValue::Number(tag)),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("tag access on a non-enum comp value"),
                )),
            },
            CheckedProjection::EnumBody { field_index, .. } => match value {
                ConstValue::Enum { fields, .. } => Ok(fields[*field_index].clone()),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("body-field access on a non-enum comp value"),
                )),
            },
            CheckedProjection::EnumHeader { index, .. } => match value {
                ConstValue::Enum { header, .. } => Ok(header[*index].clone()),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("header access on a non-enum comp value"),
                )),
            },
            CheckedProjection::EnumDynamicField { index, .. } => match value {
                ConstValue::Enum { dynamic_fields, .. } => Ok(dynamic_fields[*index].clone()),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("dynamic-field access on a non-enum comp value"),
                )),
            },
            CheckedProjection::UnionField { index, .. } => match value {
                ConstValue::Union { field_index, value } if field_index == *index => Ok(*value),
                ConstValue::Union { .. } => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("reading a union through its inactive field"),
                )),
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("field access on a non-union comp value"),
                )),
            },
            CheckedProjection::SpecObjectPtr { .. } | CheckedProjection::SpecObjectVtable => {
                Err(self.err(
                    span,
                    CompErrorKind::Unsupported(
                        "accessing a spec object's pointer/vtable inside a 'comp' evaluation",
                    ),
                ))
            }
        }
    }

    fn write_place(
        &mut self,
        place: &CheckedPlace,
        value: ConstValue,
        span: Span,
    ) -> CompResult<()> {
        if place.projections.is_empty() {
            return self.write_root(&place.root, value, span);
        }
        // A projected write (`a.b = x`, `a[i] = x`) reads the whole root
        // value, mutates the projected-into leaf, and writes the whole
        // value back -- there's no real memory to mutate through, only a
        // `ConstValue` tree, so "mutate a leaf" means "rebuild the tree."
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
                _ => Err(self.err(
                    span,
                    CompErrorKind::Unsupported("field write on a non-struct comp value"),
                )),
            },
            CheckedProjection::Index { index_expr, .. } => {
                let index = self.expect_index(index_expr)?;
                match base {
                    ConstValue::Array(mut v) => {
                        if index >= v.len() {
                            return Err(self.err(
                                span,
                                CompErrorKind::Unsupported("an out-of-range comp index write"),
                            ));
                        }
                        let inner = std::mem::replace(&mut v[index], ConstValue::Bool(false));
                        v[index] = self.write_projections(inner, rest, value, span)?;
                        Ok(ConstValue::Array(v))
                    }
                    _ => Err(self.err(
                        span,
                        CompErrorKind::Unsupported("index write on a non-array comp value"),
                    )),
                }
            }
            CheckedProjection::UnionField { index, .. } => {
                let inner = self.write_projections(ConstValue::Bool(false), rest, value, span)?;
                Ok(ConstValue::Union {
                    field_index: *index,
                    value: Box::new(inner),
                })
            }
            CheckedProjection::Deref { .. } => {
                Err(self.err(span, CompErrorKind::UnresolvableMemory))
            }
            CheckedProjection::SpecObjectPtr { .. } | CheckedProjection::SpecObjectVtable => {
                Err(self.err(
                    span,
                    CompErrorKind::Unsupported(
                        "writing through a spec object's pointer/vtable inside a 'comp' evaluation",
                    ),
                ))
            }
            _ => Err(self.err(
                span,
                CompErrorKind::Unsupported("this write target inside a comp evaluation"),
            )),
        }
    }

    fn write_root(
        &mut self,
        root: &CheckedPlaceRoot,
        value: ConstValue,
        span: Span,
    ) -> CompResult<()> {
        match root {
            CheckedPlaceRoot::Variable {
                decl_id,
                storage: Storage::Local | Storage::Parameter,
                ..
            } => {
                self.frame().locals.insert(*decl_id, value);
                Ok(())
            }
            CheckedPlaceRoot::Variable {
                storage: Storage::Global,
                ..
            } => Err(self.err(span, CompErrorKind::NonCompGlobalRead)),
            _ => Err(self.err(
                span,
                CompErrorKind::Unsupported("this assignment target inside a comp evaluation"),
            )),
        }
    }
}

enum BlockResult {
    Value(ConstValue),
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

fn cast_number(n: NumberValue, target: crate::resolved_type::NumericKind) -> NumberValue {
    use crate::resolved_type::NumericKind;
    let mask = |width: u32, bits: u64| {
        if width >= 64 {
            bits
        } else {
            bits & ((1u64 << width) - 1)
        }
    };
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
            NumberValue::Float(if width == 32 {
                as_f64 as f32 as f64
            } else {
                as_f64
            })
        }
    }
}

#[cfg(test)]
mod tests;
