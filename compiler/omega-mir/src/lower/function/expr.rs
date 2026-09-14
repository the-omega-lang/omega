use super::FunctionLowerer;
use crate::body::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall,
    MirEnumConstruct, MirExpr, MirExprNode, MirFieldInit, MirFunctionCall, MirSlice, MirSpecCoerce,
    MirStructLiteral, MirTerminator, MirUnionConstruct,
};
use omega_analyzer::checked::{
    CheckedExpr, CheckedExprNode, CheckedRangeEnd, CheckedStructLiteralField,
};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_analyzer::runtime_checks::RuntimeCheck;
use omega_hir::HirId;
use omega_parser::prelude::Span;

pub(super) fn lower_expr(lowerer: &mut FunctionLowerer, node: CheckedExprNode) -> MirExprNode {
    let CheckedExprNode {
        id,
        span,
        r#type,
        kind,
    } = node;

    // Something already lowered in this block terminated it -- an unexpected
    // return from a `never` call is the only way an ordinary expression does
    // that. Whatever would have run after it does not run, so nothing after
    // it is emitted either.
    if lowerer.is_current_terminated() {
        return lowerer.unreachable_value(id, span, r#type);
    }

    match kind {
        CheckedExpr::If(if_expr) => {
            lowerer.lower_if_expr(id, span, r#type, if_expr.branches, if_expr.else_branch)
        }
        CheckedExpr::Match(match_expr) => lowerer.lower_match_expr(
            id,
            span,
            r#type,
            match_expr.arms,
            match_expr.else_branch,
            match_expr.remainder,
        ),
        CheckedExpr::Codeblock(block) => lowerer.lower_codeblock_expr(id, span, r#type, block),
        CheckedExpr::Try(r#try) => lowerer.lower_try_expr(id, span, r#type, r#try),
        CheckedExpr::Place(place) => {
            let place = lowerer.lower_place(place);
            mir_node(id, span, r#type, MirExpr::Place(place))
        }
        CheckedExpr::Number(value) => mir_node(id, span, r#type, MirExpr::Number(value)),
        CheckedExpr::Bool(value) => mir_node(id, span, r#type, MirExpr::Bool(value)),
        CheckedExpr::Char(value) => mir_node(id, span, r#type, MirExpr::Char(value)),
        CheckedExpr::String(value) => mir_node(id, span, r#type, MirExpr::String(value)),
        CheckedExpr::ByteString(value) => mir_node(id, span, r#type, MirExpr::ByteString(value)),
        CheckedExpr::Const(value) => mir_node(id, span, r#type, MirExpr::Const(value)),
        CheckedExpr::Sizeof(ty) => mir_node(id, span, r#type, MirExpr::Sizeof(ty)),
        CheckedExpr::Alignof(ty) => mir_node(id, span, r#type, MirExpr::Alignof(ty)),
        CheckedExpr::FunctionCall(call) => {
            let trusted = lowerer.is_trusted_panic_handler(&call.callee);
            let mut operands =
                lower_sequence(lowerer, std::iter::once(*call.callee).chain(call.args)).into_iter();
            let callee = Box::new(operands.next().expect("a call has a callee"));
            let args = operands.collect();
            let fn_type = call.fn_type;
            let node = mir_node(
                id,
                span,
                r#type.clone(),
                MirExpr::FunctionCall(MirFunctionCall {
                    callee,
                    fn_type: fn_type.clone(),
                    args,
                }),
            );
            guard_never_call(lowerer, id, span, r#type, &fn_type, trusted, node)
        }
        CheckedExpr::Assignment(assignment) => {
            let target = lowerer.lower_place(assignment.target);
            let value = Box::new(lowerer.lower_expr(*assignment.value));
            mir_node(
                id,
                span,
                r#type,
                MirExpr::Assignment(MirAssignment { target, value }),
            )
        }
        CheckedExpr::CompoundAssign(compound) => {
            lower_compound_assign(lowerer, id, span, r#type, compound)
        }
        CheckedExpr::AddressOf(address_of) => {
            let place = lowerer.lower_place(address_of.place);
            mir_node(id, span, r#type, MirExpr::AddressOf(MirAddressOf { place }))
        }
        CheckedExpr::Negate(inner) => mir_node(
            id,
            span,
            r#type,
            MirExpr::Negate(Box::new(lowerer.lower_expr(*inner))),
        ),
        CheckedExpr::BitNot(inner) => mir_node(
            id,
            span,
            r#type,
            MirExpr::BitNot(Box::new(lowerer.lower_expr(*inner))),
        ),
        CheckedExpr::BinaryOp(binary) => {
            let mut operands = lower_sequence(lowerer, [*binary.left, *binary.right]).into_iter();
            let left = Box::new(operands.next().expect("binary left operand"));
            let right = Box::new(operands.next().expect("binary right operand"));
            mir_node(
                id,
                span,
                r#type,
                MirExpr::BinaryOp(MirBinaryOp {
                    op: binary.op,
                    left,
                    right,
                }),
            )
        }
        CheckedExpr::ArrayLiteral(literal) => {
            let elements = lower_sequence(lowerer, literal.elements);
            mir_node(
                id,
                span,
                r#type,
                MirExpr::ArrayLiteral(MirArrayLiteral {
                    item_type: literal.item_type,
                    elements,
                }),
            )
        }
        CheckedExpr::StructLiteral(literal) => {
            let fields = lower_fields(lowerer, literal.fields);
            mir_node(
                id,
                span,
                r#type,
                MirExpr::StructLiteral(MirStructLiteral { fields }),
            )
        }
        CheckedExpr::EnumConstruct(construct) => {
            let fields = lower_fields(lowerer, construct.fields);
            mir_node(
                id,
                span,
                r#type,
                MirExpr::EnumConstruct(MirEnumConstruct {
                    variant_index: construct.variant_index,
                    fields,
                }),
            )
        }
        CheckedExpr::UnionConstruct(construct) => mir_node(
            id,
            span,
            r#type,
            MirExpr::UnionConstruct(MirUnionConstruct {
                field_index: construct.field_index,
                value: Box::new(lowerer.lower_expr(*construct.value)),
            }),
        ),
        CheckedExpr::Slice(slice) => {
            let base = lowerer.lower_place(slice.base);
            let start = slice
                .start
                .map(|start| Box::new(lowerer.lower_expr(*start)));
            let (end, inclusive) = match slice.end {
                CheckedRangeEnd::Inclusive(end) => (Some(Box::new(lowerer.lower_expr(*end))), true),
                CheckedRangeEnd::Exclusive(end) => {
                    (Some(Box::new(lowerer.lower_expr(*end))), false)
                }
                CheckedRangeEnd::Open => (None, false),
            };
            mir_node(
                id,
                span,
                r#type,
                MirExpr::Slice(MirSlice {
                    base,
                    item_type: slice.item_type,
                    start,
                    end,
                    inclusive,
                }),
            )
        }
        CheckedExpr::Cast(cast) => mir_node(
            id,
            span,
            r#type,
            MirExpr::Cast(MirCast {
                kind: cast.kind,
                target_type: cast.target_type,
                base: Box::new(lowerer.lower_expr(*cast.base)),
            }),
        ),
        CheckedExpr::AnonymousEnumWiden(widen) => {
            lowerer.lower_anonymous_enum_widen(id, span, r#type, widen)
        }
        CheckedExpr::SpecCoerce(coerce) => mir_node(
            id,
            span,
            r#type,
            MirExpr::SpecCoerce(MirSpecCoerce {
                base: Box::new(lowerer.lower_expr(*coerce.base)),
                slots: coerce.slots,
            }),
        ),
        CheckedExpr::DynamicCall(call) => {
            let base = CheckedExprNode {
                id,
                span,
                r#type: call.base.r#type.clone(),
                kind: CheckedExpr::Place(call.base),
            };
            let mut operands =
                lower_sequence(lowerer, std::iter::once(base).chain(call.args)).into_iter();
            let MirExpr::Place(base) = operands.next().expect("a dynamic call has a base").kind
            else {
                unreachable!("a place is either preserved or materialized into a local")
            };
            let args = operands.collect();
            let fn_type = call.fn_type;
            let node = mir_node(
                id,
                span,
                r#type.clone(),
                MirExpr::DynamicCall(MirDynamicCall {
                    base,
                    slot_index: call.slot_index,
                    fn_type: fn_type.clone(),
                    args,
                }),
            );
            guard_never_call(lowerer, id, span, r#type, &fn_type, false, node)
        }
    }
}

/// A call whose Omega result is `never` must not come back. The call itself
/// is emitted for its effects and the block ends immediately after it, so an
/// unexpected return reports the broken contract instead of continuing into
/// whatever the surrounding expression would have done with a value that was
/// never produced.
///
/// The canonical panic handler is the one exception: it is the operation this
/// check reports *through*, so guarding it would make every generated panic
/// instrument itself.
fn guard_never_call(
    lowerer: &mut FunctionLowerer,
    id: HirId,
    span: Span,
    r#type: ResolvedType,
    fn_type: &ResolvedFunctionType,
    trusted: bool,
    call: MirExprNode,
) -> MirExprNode {
    if *fn_type.return_type != ResolvedType::Never || lowerer.is_current_terminated() {
        return call;
    }
    lowerer.push_stmt(call);
    if trusted {
        lowerer.terminate(MirTerminator::Unreachable);
    } else {
        lowerer.emit_runtime_panic(id, span, RuntimeCheck::NeverCallReturned);
    }
    lowerer.unreachable_value(id, span, r#type)
}

fn lower_compound_assign(
    lowerer: &mut FunctionLowerer,
    id: HirId,
    span: Span,
    r#type: ResolvedType,
    compound: omega_analyzer::checked::CheckedCompoundAssign,
) -> MirExprNode {
    // Lowered once here; `target` is reused below for both the read and the
    // write, so any dynamic index/root expression it contains executes
    // exactly once (see `lower_place_evaluated_once`).
    let target = lowerer.lower_place_evaluated_once(compound.place);
    let mut read = mir_node(
        id,
        span,
        target.r#type.clone(),
        MirExpr::Place(target.clone()),
    );
    if let Some((kind, target_type)) = compound.read_cast {
        read = mir_node(
            id,
            span,
            target_type.clone(),
            MirExpr::Cast(MirCast {
                kind,
                target_type,
                base: Box::new(read),
            }),
        );
    }
    let value = Box::new(lowerer.lower_expr(*compound.value));
    let combined = mir_node(
        id,
        span,
        compound.result_type,
        MirExpr::BinaryOp(MirBinaryOp {
            op: compound.op,
            left: Box::new(read),
            right: value,
        }),
    );
    mir_node(
        id,
        span,
        r#type,
        MirExpr::Assignment(MirAssignment {
            target,
            value: Box::new(combined),
        }),
    )
}

// Lowering a later operand can emit CFG immediately, while earlier operands
// still exist only as expression trees. Save those operands before that CFG;
// otherwise a panic can skip their effects or a branch can change their reads.
fn lower_sequence(
    lowerer: &mut FunctionLowerer,
    nodes: impl IntoIterator<Item = CheckedExprNode>,
) -> Vec<MirExprNode> {
    let mut values: Vec<MirExprNode> = Vec::new();
    let mut pending = 0;
    for node in nodes {
        let block = lowerer.current;
        let position = lowerer.current_block().statements.len();
        let live = !lowerer.is_current_terminated();
        let value = lowerer.lower_expr(node);
        let previous = &lowerer.blocks[block.index()];
        if live && (previous.terminator.is_some() || previous.statements.len() != position) {
            let mut stores = Vec::new();
            for operand in &mut values[pending..] {
                let local = lowerer.declare_local(None, operand.r#type.clone());
                let saved = lowerer.local_expr(local, operand.id, operand.span);
                let operand = std::mem::replace(operand, saved);
                stores.push(mir_node(
                    operand.id,
                    operand.span,
                    operand.r#type.clone(),
                    MirExpr::Assignment(MirAssignment {
                        target: lowerer.local_place(local),
                        value: Box::new(operand),
                    }),
                ));
            }
            lowerer.blocks[block.index()]
                .statements
                .splice(position..position, stores);
            pending = values.len();
        }
        values.push(value);
    }
    values
}

fn lower_fields(
    lowerer: &mut FunctionLowerer,
    fields: Vec<CheckedStructLiteralField>,
) -> Vec<MirFieldInit> {
    let (indices, values): (Vec<_>, Vec<_>) = fields
        .into_iter()
        .map(|field| (field.field_index, field.value))
        .unzip();
    indices
        .into_iter()
        .zip(lower_sequence(lowerer, values))
        .map(|(field_index, value)| MirFieldInit { field_index, value })
        .collect()
}

fn mir_node(id: HirId, span: Span, r#type: ResolvedType, kind: MirExpr) -> MirExprNode {
    MirExprNode {
        id,
        span,
        r#type,
        kind,
    }
}
