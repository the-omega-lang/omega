use crate::CodegenRequest;
use omega_analyzer::resolved_type::ResolvedType;
use omega_diagnostics::Span;
use omega_mir::{
    MirBody, MirExpr, MirExprNode, MirItem, MirPlace, MirPlaceRoot, MirProjection, MirTerminator,
};

pub(crate) fn preflight(request: &CodegenRequest) -> Result<(), String> {
    for (_, module) in &request.modules {
        for item in &module.items {
            match item {
                // Extern data still has no supported storage model; reject it uniformly before backend selection.
                MirItem::ExternDeclaration(decl)
                    if !matches!(decl.r#type, ResolvedType::Function(_)) =>
                {
                    return Err(format!(
                        "extern data declarations (a non-function `extern`) are not implemented yet: '{}'",
                        decl.ident.as_ref()
                    ));
                }
                MirItem::FunctionDefinition(f) => {
                    if let Some(span) = parameter_assignment(&f.body) {
                        return Err(parameter_assignment_error(span));
                    }
                }
                MirItem::Struct(s) => {
                    for f in &s.functions {
                        if let Some(span) = parameter_assignment(&f.body) {
                            return Err(parameter_assignment_error(span));
                        }
                    }
                }
                MirItem::Enum(e) => {
                    for f in &e.functions {
                        if let Some(span) = parameter_assignment(&f.body) {
                            return Err(parameter_assignment_error(span));
                        }
                    }
                }
                MirItem::Union(u) => {
                    for f in &u.functions {
                        if let Some(span) = parameter_assignment(&f.body) {
                            return Err(parameter_assignment_error(span));
                        }
                    }
                }
                MirItem::Declaration(_) | MirItem::ExternDeclaration(_) => {}
            }
        }
    }
    Ok(())
}

fn parameter_assignment_error(span: Span) -> String {
    format!(
        "assignment into a function parameter is not implemented yet (source offset {})",
        span.start
    )
}

fn parameter_assignment(body: &MirBody) -> Option<Span> {
    for block in &body.blocks {
        for stmt in &block.statements {
            if let Some(span) = expr_parameter_assignment(stmt, body.arg_count) {
                return Some(span);
            }
        }
        if let Some(span) = terminator_parameter_assignment(&block.terminator, body.arg_count) {
            return Some(span);
        }
    }
    None
}

fn terminator_parameter_assignment(terminator: &MirTerminator, arg_count: usize) -> Option<Span> {
    match terminator {
        MirTerminator::Branch { condition, .. } => expr_parameter_assignment(condition, arg_count),
        MirTerminator::Return(Some(expr)) => expr_parameter_assignment(expr, arg_count),
        MirTerminator::Return(None) | MirTerminator::Goto(_) | MirTerminator::Unreachable => None,
    }
}

fn expr_parameter_assignment(expr: &MirExprNode, arg_count: usize) -> Option<Span> {
    match &expr.kind {
        MirExpr::Assignment(assignment) => {
            // Point the diagnostic at the unsupported assignment target, not the whole expression.
            if place_targets_parameter(&assignment.target, arg_count) {
                return Some(expr.span);
            }
            expr_parameter_assignment(&assignment.value, arg_count)
        }
        MirExpr::Place(place) => place_nested_assignment(place, arg_count),
        MirExpr::FunctionCall(call) => {
            expr_parameter_assignment(&call.callee, arg_count).or_else(|| {
                call.args
                    .iter()
                    .find_map(|arg| expr_parameter_assignment(arg, arg_count))
            })
        }
        MirExpr::AddressOf(address_of) => place_nested_assignment(&address_of.place, arg_count),
        MirExpr::Negate(inner) | MirExpr::BitNot(inner) => {
            expr_parameter_assignment(inner, arg_count)
        }
        MirExpr::BinaryOp(op) => expr_parameter_assignment(&op.left, arg_count)
            .or_else(|| expr_parameter_assignment(&op.right, arg_count)),
        MirExpr::ArrayLiteral(literal) => literal
            .elements
            .iter()
            .find_map(|element| expr_parameter_assignment(element, arg_count)),
        MirExpr::StructLiteral(literal) => literal
            .fields
            .iter()
            .find_map(|field| expr_parameter_assignment(&field.value, arg_count)),
        MirExpr::EnumConstruct(construct) => construct
            .fields
            .iter()
            .find_map(|field| expr_parameter_assignment(&field.value, arg_count)),
        MirExpr::UnionConstruct(construct) => {
            expr_parameter_assignment(&construct.value, arg_count)
        }
        MirExpr::Slice(slice) => place_nested_assignment(&slice.base, arg_count)
            .or_else(|| {
                slice
                    .start
                    .as_ref()
                    .and_then(|start| expr_parameter_assignment(start, arg_count))
            })
            .or_else(|| {
                slice
                    .end
                    .as_ref()
                    .and_then(|end| expr_parameter_assignment(end, arg_count))
            }),
        MirExpr::Cast(cast) => expr_parameter_assignment(&cast.base, arg_count),
        MirExpr::SpecCoerce(coerce) => expr_parameter_assignment(&coerce.base, arg_count),
        MirExpr::DynamicCall(call) => {
            place_nested_assignment(&call.base, arg_count).or_else(|| {
                call.args
                    .iter()
                    .find_map(|arg| expr_parameter_assignment(arg, arg_count))
            })
        }
        MirExpr::Number(_)
        | MirExpr::Bool(_)
        | MirExpr::Char(_)
        | MirExpr::String(_)
        | MirExpr::ByteString(_)
        | MirExpr::Sizeof(_)
        | MirExpr::Const(_) => None,
    }
}

fn place_targets_parameter(place: &MirPlace, arg_count: usize) -> bool {
    matches!(&place.root, MirPlaceRoot::Local { id, .. } if id.index() < arg_count)
        && !place
            .projections
            .iter()
            .any(|p| matches!(p, MirProjection::Deref { .. } | MirProjection::Index { .. }))
}

fn place_nested_assignment(place: &MirPlace, arg_count: usize) -> Option<Span> {
    for projection in &place.projections {
        if let MirProjection::Index { index_expr, .. } = projection
            && let Some(span) = expr_parameter_assignment(index_expr, arg_count)
        {
            return Some(span);
        }
    }
    None
}
