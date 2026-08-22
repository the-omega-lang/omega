use omega_analyzer::checked::{
    CheckedBlock, CheckedExpr, CheckedExprNode, CheckedPlace, CheckedPlaceRoot, CheckedProjection,
    CheckedRangeEnd, CheckedStmt,
};
use omega_hir::HirId;
use omega_parser::prelude::Span;

pub(super) fn collect_defer_ids(block: &CheckedBlock) -> Vec<(HirId, Span)> {
    let mut defers = Vec::new();
    collect_block(block, &mut defers);
    defers
}

fn collect_block(block: &CheckedBlock, out: &mut Vec<(HirId, Span)>) {
    for stmt in &block.stmts {
        collect_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        collect_expr(tail, out);
    }
}

fn collect_stmt(stmt: &CheckedStmt, out: &mut Vec<(HirId, Span)>) {
    match stmt {
        CheckedStmt::Declaration(_)
        | CheckedStmt::Break(_)
        | CheckedStmt::Continue(_)
        | CheckedStmt::InlineAsm(_) => {}
        CheckedStmt::Expression(expr) | CheckedStmt::Return(expr) => collect_expr(expr, out),
        CheckedStmt::While(while_loop) => {
            collect_expr(&while_loop.condition, out);
            collect_block(&while_loop.body, out);
        }
        CheckedStmt::Loop(loop_stmt) => collect_block(&loop_stmt.body, out),
        CheckedStmt::For(for_loop) => {
            for stmt in &for_loop.init {
                collect_stmt(stmt, out);
            }
            collect_expr(&for_loop.condition, out);
            if let Some(post) = &for_loop.post {
                collect_expr(post, out);
            }
            collect_block(&for_loop.body, out);
        }
        CheckedStmt::Defer(defer) => {
            out.push((defer.id, defer.span));
            // Nested defers are rejected earlier, but recurse so this pre-pass remains a complete
            // structural visitor if that restriction changes later.
            collect_block(&defer.body, out);
        }
    }
}

fn collect_expr(expr: &CheckedExprNode, out: &mut Vec<(HirId, Span)>) {
    match &expr.kind {
        CheckedExpr::Number(_)
        | CheckedExpr::Bool(_)
        | CheckedExpr::Char(_)
        | CheckedExpr::String(_)
        | CheckedExpr::ByteString(_)
        | CheckedExpr::Const(_)
        | CheckedExpr::Sizeof(_) => {}
        CheckedExpr::Place(place) => collect_place(place, out),
        CheckedExpr::FunctionCall(call) => {
            collect_expr(&call.callee, out);
            for arg in &call.args {
                collect_expr(arg, out);
            }
        }
        CheckedExpr::Assignment(assignment) => {
            collect_place(&assignment.target, out);
            collect_expr(&assignment.value, out);
        }
        CheckedExpr::CompoundAssign(compound) => {
            collect_place(&compound.place, out);
            collect_expr(&compound.value, out);
        }
        CheckedExpr::AddressOf(address_of) => collect_place(&address_of.place, out),
        CheckedExpr::Negate(inner) | CheckedExpr::BitNot(inner) => collect_expr(inner, out),
        CheckedExpr::BinaryOp(binary) => {
            collect_expr(&binary.left, out);
            collect_expr(&binary.right, out);
        }
        CheckedExpr::Codeblock(block) => collect_block(block, out),
        CheckedExpr::If(if_expr) => {
            for (condition, block) in &if_expr.branches {
                collect_expr(condition, out);
                collect_block(block, out);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                collect_block(else_branch, out);
            }
        }
        CheckedExpr::ArrayLiteral(literal) => {
            for element in &literal.elements {
                collect_expr(element, out);
            }
        }
        CheckedExpr::StructLiteral(literal) => {
            for field in &literal.fields {
                collect_expr(&field.value, out);
            }
        }
        CheckedExpr::EnumConstruct(construct) => {
            for field in &construct.fields {
                collect_expr(&field.value, out);
            }
        }
        CheckedExpr::UnionConstruct(construct) => collect_expr(&construct.value, out),
        CheckedExpr::Slice(slice) => {
            collect_place(&slice.base, out);
            if let Some(start) = &slice.start {
                collect_expr(start, out);
            }
            match &slice.end {
                CheckedRangeEnd::Inclusive(end) | CheckedRangeEnd::Exclusive(end) => {
                    collect_expr(end, out)
                }
                CheckedRangeEnd::Open => {}
            }
        }
        CheckedExpr::Match(match_expr) => {
            for arm in &match_expr.arms {
                for group in &arm.conditions {
                    for condition in group {
                        collect_expr(condition, out);
                    }
                }
                collect_block(&arm.body, out);
            }
            if let Some(else_branch) = &match_expr.else_branch {
                collect_block(else_branch, out);
            }
        }
        CheckedExpr::Cast(cast) => collect_expr(&cast.base, out),
        CheckedExpr::SpecCoerce(coerce) => collect_expr(&coerce.base, out),
        CheckedExpr::DynamicCall(call) => {
            collect_place(&call.base, out);
            for arg in &call.args {
                collect_expr(arg, out);
            }
        }
    }
}

fn collect_place(place: &CheckedPlace, out: &mut Vec<(HirId, Span)>) {
    if let CheckedPlaceRoot::Expr(expr) = &place.root {
        collect_expr(expr, out);
    }
    for projection in &place.projections {
        if let CheckedProjection::Index { index_expr, .. } = projection {
            collect_expr(index_expr, out);
        }
    }
}
