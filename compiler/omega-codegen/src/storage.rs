use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::{
    MirBody, MirExpr, MirExprNode, MirPlace, MirPlaceRoot, MirProjection, MirTerminator,
};

/// Backend-neutral function-local storage decisions.
///
/// Most parameters can remain in backend SSA values for the whole function. A
/// parameter only needs a stable stack home when the program can observe or
/// mutate the parameter's own storage. Computing that once from MIR keeps both
/// backends on the same rule without forcing every parameter through memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParameterHome {
    Ssa,
    Stack,
}

pub(crate) fn parameter_storage_plan(body: &MirBody) -> Vec<ParameterHome> {
    let mut homes = vec![ParameterHome::Ssa; body.arg_count];

    for block in &body.blocks {
        for statement in &block.statements {
            scan_expr(statement, body.arg_count, &mut homes);
        }
        scan_terminator(&block.terminator, body.arg_count, &mut homes);
    }

    homes
}

fn scan_terminator(terminator: &MirTerminator, arg_count: usize, homes: &mut [ParameterHome]) {
    match terminator {
        MirTerminator::Branch { condition, .. } => {
            scan_expr(condition, arg_count, homes);
        }
        MirTerminator::Return(Some(value)) => scan_expr(value, arg_count, homes),
        MirTerminator::Return(None) | MirTerminator::Goto(_) | MirTerminator::Unreachable => {}
    }
}

fn scan_expr(expr: &MirExprNode, arg_count: usize, homes: &mut [ParameterHome]) {
    match &expr.kind {
        MirExpr::Place(place) => scan_place_exprs(place, arg_count, homes),
        MirExpr::Assignment(assignment) => {
            mark_parameter_storage(&assignment.target, arg_count, homes);
            scan_place_exprs(&assignment.target, arg_count, homes);
            scan_expr(&assignment.value, arg_count, homes);
        }
        MirExpr::AddressOf(address_of) => {
            mark_parameter_storage(&address_of.place, arg_count, homes);
            scan_place_exprs(&address_of.place, arg_count, homes);
        }
        MirExpr::FunctionCall(call) => {
            scan_expr(&call.callee, arg_count, homes);
            for arg in &call.args {
                scan_expr(arg, arg_count, homes);
            }
        }
        MirExpr::Negate(inner) | MirExpr::BitNot(inner) => {
            scan_expr(inner, arg_count, homes);
        }
        MirExpr::BinaryOp(binary) => {
            scan_expr(&binary.left, arg_count, homes);
            scan_expr(&binary.right, arg_count, homes);
        }
        MirExpr::ArrayLiteral(literal) => {
            for element in &literal.elements {
                scan_expr(element, arg_count, homes);
            }
        }
        MirExpr::StructLiteral(literal) => {
            for field in &literal.fields {
                scan_expr(&field.value, arg_count, homes);
            }
        }
        MirExpr::EnumConstruct(construct) => {
            for field in &construct.fields {
                scan_expr(&field.value, arg_count, homes);
            }
        }
        MirExpr::UnionConstruct(construct) => {
            scan_expr(&construct.value, arg_count, homes);
        }
        MirExpr::Slice(slice) => {
            if matches!(&slice.base.r#type, ResolvedType::SizedArray(_, _)) {
                mark_parameter_storage(&slice.base, arg_count, homes);
            }
            scan_place_exprs(&slice.base, arg_count, homes);
            if let Some(start) = &slice.start {
                scan_expr(start, arg_count, homes);
            }
            if let Some(end) = &slice.end {
                scan_expr(end, arg_count, homes);
            }
        }
        MirExpr::Cast(cast) => scan_expr(&cast.base, arg_count, homes),
        MirExpr::SpecCoerce(coerce) => scan_expr(&coerce.base, arg_count, homes),
        MirExpr::DynamicCall(call) => {
            scan_place_exprs(&call.base, arg_count, homes);
            for arg in &call.args {
                scan_expr(arg, arg_count, homes);
            }
        }
        MirExpr::Number(_)
        | MirExpr::Bool(_)
        | MirExpr::Char(_)
        | MirExpr::String(_)
        | MirExpr::ByteString(_)
        | MirExpr::Sizeof(_)
        | MirExpr::Const(_) => {}
    }
}

fn scan_place_exprs(place: &MirPlace, arg_count: usize, homes: &mut [ParameterHome]) {
    if let MirPlaceRoot::Expr(expr) = &place.root {
        scan_expr(expr, arg_count, homes);
    }
    for projection in &place.projections {
        if let MirProjection::Index { index_expr, .. } = projection {
            scan_expr(index_expr, arg_count, homes);
        }
    }
}

fn mark_parameter_storage(place: &MirPlace, arg_count: usize, homes: &mut [ParameterHome]) {
    if let Some(index) = parameter_storage_owner(place, arg_count) {
        homes[index] = ParameterHome::Stack;
    }
}

/// Returns the parameter whose own value storage contains the final place.
/// Dereferencing a pointer or indexing a pointer-backed sequence crosses out
/// of the parameter itself, so writes/address-taking beyond that boundary do
/// not require spilling the parameter value.
fn parameter_storage_owner(place: &MirPlace, arg_count: usize) -> Option<usize> {
    let MirPlaceRoot::Local { id, r#type } = &place.root else {
        return None;
    };
    let parameter = id.index();
    if parameter >= arg_count {
        return None;
    }

    let mut current_type = r#type.clone();
    for projection in &place.projections {
        match projection {
            MirProjection::Deref { .. } => return None,
            MirProjection::Index { item_type, .. } => match current_type {
                ResolvedType::SizedArray(_, _) => current_type = item_type.clone(),
                ResolvedType::Array(_, _)
                | ResolvedType::Slice { .. }
                | ResolvedType::Str { .. } => return None,
                _ => return None,
            },
            MirProjection::FieldAccess { r#type, .. }
            | MirProjection::UnionField { r#type, .. }
            | MirProjection::EnumTag { r#type }
            | MirProjection::EnumHeader { r#type, .. }
            | MirProjection::EnumDynamicField { r#type, .. }
            | MirProjection::EnumBody { r#type, .. } => current_type = r#type.clone(),
            MirProjection::SliceLength => current_type = ResolvedType::I32,
            MirProjection::SpecObjectPtr { mutable } => {
                current_type = ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::U8),
                    mutable: *mutable,
                };
            }
            MirProjection::SpecObjectVtable => {
                current_type = ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::U8),
                    mutable: false,
                };
            }
        }
    }

    Some(parameter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_analyzer::checked::NumberValue;
    use omega_mir::LocalId;

    fn local(id: u32, r#type: ResolvedType, projections: Vec<MirProjection>) -> MirPlace {
        MirPlace {
            root: MirPlaceRoot::Local {
                id: LocalId(id),
                r#type: r#type.clone(),
            },
            projections,
            r#type,
            align: 1,
        }
    }

    fn node(kind: MirExpr, r#type: ResolvedType) -> MirExprNode {
        MirExprNode {
            id: omega_hir::HirId {
                module: omega_hir::ModuleId(0),
                local: 0,
            },
            span: Default::default(),
            r#type,
            kind,
        }
    }

    fn body(parameter_type: ResolvedType, statement: MirExprNode) -> MirBody {
        MirBody {
            locals: vec![omega_mir::MirLocalDecl {
                source: None,
                r#type: parameter_type,
            }],
            arg_count: 1,
            blocks: vec![omega_mir::MirBlockData {
                statements: vec![statement],
                terminator: MirTerminator::Return(None),
            }],
        }
    }

    #[test]
    fn direct_parameter_assignment_requires_a_stable_home() {
        let parameter = local(0, ResolvedType::Bool, vec![]);
        let assignment = node(
            MirExpr::Assignment(omega_mir::MirAssignment {
                target: parameter,
                value: Box::new(node(MirExpr::Bool(true), ResolvedType::Bool)),
            }),
            ResolvedType::Bool,
        );
        assert_eq!(
            parameter_storage_plan(&body(ResolvedType::Bool, assignment)),
            vec![ParameterHome::Stack]
        );
    }

    #[test]
    fn taking_parameter_address_requires_a_stable_home() {
        let parameter = local(0, ResolvedType::I32, vec![]);
        let address = node(
            MirExpr::AddressOf(omega_mir::MirAddressOf { place: parameter }),
            ResolvedType::Pointer {
                pointee: Box::new(ResolvedType::I32),
                mutable: false,
            },
        );
        assert_eq!(
            parameter_storage_plan(&body(ResolvedType::I32, address)),
            vec![ParameterHome::Stack]
        );
    }

    #[test]
    fn inline_array_element_assignment_requires_parameter_storage() {
        let array_type = ResolvedType::SizedArray(Box::new(ResolvedType::I32), 4);
        let parameter = local(
            0,
            array_type.clone(),
            vec![MirProjection::Index {
                index_expr: Box::new(node(
                    MirExpr::Number(NumberValue::Signed(0)),
                    ResolvedType::I32,
                )),
                item_type: ResolvedType::I32,
            }],
        );
        let assignment = node(
            MirExpr::Assignment(omega_mir::MirAssignment {
                target: parameter,
                value: Box::new(node(
                    MirExpr::Number(NumberValue::Signed(1)),
                    ResolvedType::I32,
                )),
            }),
            ResolvedType::I32,
        );
        assert_eq!(
            parameter_storage_plan(&body(array_type, assignment)),
            vec![ParameterHome::Stack]
        );
    }

    #[test]
    fn assignment_through_pointer_parameter_keeps_parameter_in_ssa() {
        let pointer_type = ResolvedType::Pointer {
            pointee: Box::new(ResolvedType::I32),
            mutable: true,
        };
        let pointee = local(
            0,
            pointer_type.clone(),
            vec![MirProjection::Deref {
                r#type: ResolvedType::I32,
            }],
        );
        let assignment = node(
            MirExpr::Assignment(omega_mir::MirAssignment {
                target: pointee,
                value: Box::new(node(
                    MirExpr::Number(NumberValue::Signed(1)),
                    ResolvedType::I32,
                )),
            }),
            ResolvedType::I32,
        );
        assert_eq!(
            parameter_storage_plan(&body(pointer_type, assignment)),
            vec![ParameterHome::Ssa]
        );
    }

    #[test]
    fn direct_parameter_place_belongs_to_parameter_storage() {
        assert_eq!(
            parameter_storage_owner(&local(0, ResolvedType::I32, vec![]), 1),
            Some(0)
        );
        assert_eq!(
            parameter_storage_owner(&local(1, ResolvedType::I32, vec![]), 1),
            None
        );
    }

    #[test]
    fn dereference_crosses_parameter_storage_boundary() {
        let place = local(
            0,
            ResolvedType::Pointer {
                pointee: Box::new(ResolvedType::I32),
                mutable: true,
            },
            vec![MirProjection::Deref {
                r#type: ResolvedType::I32,
            }],
        );
        assert_eq!(parameter_storage_owner(&place, 1), None);
    }
}
