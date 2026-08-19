use super::FunctionLowerer;
use crate::body::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall,
    MirEnumConstruct, MirExpr, MirExprNode, MirFieldInit, MirFunctionCall, MirSlice, MirSpecCoerce,
    MirStructLiteral, MirUnionConstruct,
};
use omega_analyzer::checked::{
    CheckedExpr, CheckedExprNode, CheckedRangeEnd, CheckedStructLiteralField,
};
use omega_analyzer::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::Span;

pub(super) fn lower_expr(lowerer: &mut FunctionLowerer, node: CheckedExprNode) -> MirExprNode {
    let CheckedExprNode {
        id,
        span,
        r#type,
        kind,
    } = node;

    match kind {
        CheckedExpr::If(if_expr) => {
            lowerer.lower_if_expr(id, span, r#type, if_expr.branches, if_expr.else_branch)
        }
        CheckedExpr::Match(match_expr) => {
            lowerer.lower_match_expr(id, span, r#type, match_expr.arms, match_expr.else_branch)
        }
        CheckedExpr::Codeblock(block) => lowerer.lower_codeblock_expr(id, span, r#type, block),
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
        CheckedExpr::FunctionCall(call) => {
            let callee = Box::new(lowerer.lower_expr(*call.callee));
            let args = call
                .args
                .into_iter()
                .map(|arg| lowerer.lower_expr(arg))
                .collect();
            mir_node(
                id,
                span,
                r#type,
                MirExpr::FunctionCall(MirFunctionCall {
                    callee,
                    fn_type: call.fn_type,
                    args,
                }),
            )
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
            let left = Box::new(lowerer.lower_expr(*binary.left));
            let right = Box::new(lowerer.lower_expr(*binary.right));
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
            let elements = literal
                .elements
                .into_iter()
                .map(|element| lowerer.lower_expr(element))
                .collect();
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
            let fields = literal
                .fields
                .into_iter()
                .map(|field| lower_field_init(lowerer, field))
                .collect();
            mir_node(
                id,
                span,
                r#type,
                MirExpr::StructLiteral(MirStructLiteral { fields }),
            )
        }
        CheckedExpr::EnumConstruct(construct) => {
            let fields = construct
                .fields
                .into_iter()
                .map(|field| lower_field_init(lowerer, field))
                .collect();
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
            let base = lowerer.lower_place(call.base);
            let args = call
                .args
                .into_iter()
                .map(|arg| lowerer.lower_expr(arg))
                .collect();
            mir_node(
                id,
                span,
                r#type,
                MirExpr::DynamicCall(MirDynamicCall {
                    base,
                    slot_index: call.slot_index,
                    fn_type: call.fn_type,
                    args,
                }),
            )
        }
    }
}

fn lower_field_init(
    lowerer: &mut FunctionLowerer,
    field: CheckedStructLiteralField,
) -> MirFieldInit {
    MirFieldInit {
        field_index: field.field_index,
        value: lowerer.lower_expr(field.value),
    }
}

fn mir_node(id: HirId, span: Span, r#type: ResolvedType, kind: MirExpr) -> MirExprNode {
    MirExprNode {
        id,
        span,
        r#type,
        kind,
    }
}
