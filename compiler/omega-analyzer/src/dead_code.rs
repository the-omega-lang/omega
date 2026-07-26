//! Whole-program dead-field/dead-variant usage collector -- powers
//! `AnalysisWarningKind::UnusedField`/`NeverConstructedVariant`. Unlike
//! every other warning in this crate, these two can't be decided per-item
//! (a field/variant declared in one module may only ever be touched from
//! another, and Omega has no visibility/`pub` system yet to rule that out
//! statically), so they're computed once, after the whole program's every
//! item is checked, by walking every `CheckedModule`'s every function/method
//! body and recording which `(owner_id, field_or_variant_index)` pairs are
//! actually touched. `omega_driver::Driver::compile` then diffs this against
//! each struct/union/enum cell's own declared fields/variants.
//!
//! The walk itself mirrors `omega_codegen`'s `collect_defer_ids` family (same
//! four-function recursive shape: block/stmt/expr/place) -- the two exist
//! for different reasons (this collects usage, that collects defer ids to
//! allocate) but need to visit exactly the same tree, so there's no reason
//! for their shapes to differ.

use crate::checked::{
    CheckedBlock, CheckedExpr, CheckedExprNode, CheckedItem, CheckedModule, CheckedPlace,
    CheckedPlaceRoot, CheckedProjection, CheckedStmt,
};
use crate::resolved_type::ResolvedType;
use omega_hir::HirId;
use std::collections::HashSet;

/// Every `(owner_id, index)` pair actually touched anywhere in the whole
/// compiled program, split by which kind of field/variant `index` indexes
/// into -- a struct's `fields`, a union's `fields`, an enum's
/// `dynamic_fields`, an enum variant's own body `fields` (keyed by both
/// `variant_index` and `field_index`, since two different variants' body
/// fields are entirely unrelated storage), or an enum's `variants` list
/// itself. Enum *header* fields are deliberately never tracked here -- see
/// `AnalysisWarningKind::UnusedField`'s doc comment for why they're exempt
/// from this check entirely.
#[derive(Default)]
pub struct FieldUsage {
    pub struct_fields: HashSet<(HirId, usize)>,
    pub union_fields: HashSet<(HirId, usize)>,
    pub enum_dynamic_fields: HashSet<(HirId, usize)>,
    pub enum_body_fields: HashSet<(HirId, usize, usize)>,
    pub enum_variants: HashSet<(HirId, usize)>,
}

pub fn collect_module(module: &CheckedModule, usage: &mut FieldUsage) {
    for item in &module.items {
        collect_item(item, usage);
    }
}

fn collect_item(item: &CheckedItem, usage: &mut FieldUsage) {
    match item {
        CheckedItem::Declaration(_) | CheckedItem::ExternDeclaration(_) => {}
        CheckedItem::FunctionDefinition(f) => collect_block(&f.body, usage),
        CheckedItem::Struct(s) => {
            for f in &s.functions {
                collect_block(&f.body, usage);
            }
        }
        CheckedItem::Enum(e) => {
            for f in &e.functions {
                collect_block(&f.body, usage);
            }
        }
        CheckedItem::Union(u) => {
            for f in &u.functions {
                collect_block(&f.body, usage);
            }
        }
    }
}

fn collect_block(block: &CheckedBlock, usage: &mut FieldUsage) {
    for stmt in &block.stmts {
        collect_stmt(stmt, usage);
    }
    if let Some(tail) = &block.tail {
        collect_expr(tail, usage);
    }
}

fn collect_stmt(stmt: &CheckedStmt, usage: &mut FieldUsage) {
    match stmt {
        CheckedStmt::Declaration(_) | CheckedStmt::ExternDeclaration(_) | CheckedStmt::Break(_)
        | CheckedStmt::Continue(_) => {}
        CheckedStmt::Expression(e) | CheckedStmt::Return(e) => collect_expr(e, usage),
        CheckedStmt::While(w) => {
            collect_expr(&w.condition, usage);
            collect_block(&w.body, usage);
        }
        CheckedStmt::For(f) => {
            for s in &f.init {
                collect_stmt(s, usage);
            }
            collect_expr(&f.condition, usage);
            if let Some(post) = &f.post {
                collect_expr(post, usage);
            }
            collect_block(&f.body, usage);
        }
        CheckedStmt::Defer(d) => collect_block(&d.body, usage),
    }
}

fn collect_expr(expr: &CheckedExprNode, usage: &mut FieldUsage) {
    match &expr.kind {
        CheckedExpr::Number(_)
        | CheckedExpr::Bool(_)
        | CheckedExpr::Char(_)
        | CheckedExpr::String(_)
        | CheckedExpr::ByteString(_)
        | CheckedExpr::ConstSlice(_)
        | CheckedExpr::Sizeof(_) => {}
        CheckedExpr::Place(p) => collect_place(p, usage),
        CheckedExpr::FunctionCall(call) => {
            collect_expr(&call.callee, usage);
            for arg in &call.args {
                collect_expr(arg, usage);
            }
        }
        CheckedExpr::Assignment(a) => {
            collect_place(&a.target, usage);
            collect_expr(&a.value, usage);
        }
        CheckedExpr::AddressOf(a) => collect_place(&a.place, usage),
        CheckedExpr::Negate(e) => collect_expr(e, usage),
        CheckedExpr::BitNot(e) => collect_expr(e, usage),
        CheckedExpr::BinaryOp(b) => {
            collect_expr(&b.left, usage);
            collect_expr(&b.right, usage);
        }
        CheckedExpr::Codeblock(block) => collect_block(block, usage),
        CheckedExpr::If(if_expr) => {
            for (cond, block) in &if_expr.branches {
                collect_expr(cond, usage);
                collect_block(block, usage);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                collect_block(else_branch, usage);
            }
        }
        CheckedExpr::ArrayLiteral(lit) => {
            for e in &lit.elements {
                collect_expr(e, usage);
            }
        }
        CheckedExpr::StructLiteral(lit) => {
            for f in &lit.fields {
                collect_expr(&f.value, usage);
            }
        }
        CheckedExpr::EnumConstruct(construct) => {
            if let ResolvedType::Enum { cell, .. } = &expr.r#type {
                usage.enum_variants.insert((cell.borrow().id, construct.variant_index));
            }
            for f in &construct.fields {
                collect_expr(&f.value, usage);
            }
        }
        CheckedExpr::Slice(s) => {
            collect_place(&s.base, usage);
            if let Some(start) = &s.start {
                collect_expr(start, usage);
            }
            if let Some(end) = &s.end {
                collect_expr(end, usage);
            }
        }
        CheckedExpr::Match(m) => {
            for arm in &m.arms {
                for cond in &arm.conditions {
                    collect_expr(cond, usage);
                }
                collect_block(&arm.body, usage);
            }
            if let Some(else_branch) = &m.else_branch {
                collect_block(else_branch, usage);
            }
        }
        CheckedExpr::Cast(cast) => collect_expr(&cast.base, usage),
        CheckedExpr::UnionConstruct(construct) => collect_expr(&construct.value, usage),
        CheckedExpr::SpecCoerce(coerce) => collect_expr(&coerce.base, usage),
        CheckedExpr::DynamicCall(call) => {
            collect_place(&call.base, usage);
            for arg in &call.args {
                collect_expr(arg, usage);
            }
        }
    }
}

/// Walks a place's root and every projection, tracking the resolved type
/// the *next* projection applies to as it goes -- a projection's own
/// `r#type` is the field's type (what comes after it), so identifying which
/// struct/union/enum owns the field a given projection reads requires the
/// type one step *before* it, threaded through exactly like
/// `Analyzer::analyze_place` itself threads its own running type.
fn collect_place(place: &CheckedPlace, usage: &mut FieldUsage) {
    let mut current_type = match &place.root {
        CheckedPlaceRoot::Variable { r#type, .. } => Some(r#type.clone()),
        CheckedPlaceRoot::Expr(e) => {
            collect_expr(e, usage);
            Some(e.r#type.clone())
        }
    };

    for proj in &place.projections {
        match proj {
            CheckedProjection::FieldAccess { index, r#type, .. } => {
                if let Some(ResolvedType::Struct(cell)) = &current_type {
                    usage.struct_fields.insert((cell.borrow().id, *index));
                }
                current_type = Some(r#type.clone());
            }
            CheckedProjection::UnionField { index, r#type, .. } => {
                if let Some(ResolvedType::Union(cell)) = &current_type {
                    usage.union_fields.insert((cell.borrow().id, *index));
                }
                current_type = Some(r#type.clone());
            }
            CheckedProjection::EnumDynamicField { index, r#type, .. } => {
                if let Some(ResolvedType::Enum { cell, .. }) = &current_type {
                    usage.enum_dynamic_fields.insert((cell.borrow().id, *index));
                }
                current_type = Some(r#type.clone());
            }
            CheckedProjection::EnumBody { variant_index, field_index, r#type, .. } => {
                if let Some(ResolvedType::Enum { cell, .. }) = &current_type {
                    usage.enum_body_fields.insert((cell.borrow().id, *variant_index, *field_index));
                }
                current_type = Some(r#type.clone());
            }
            CheckedProjection::Index { index_expr, item_type } => {
                collect_expr(index_expr, usage);
                current_type = Some(item_type.clone());
            }
            CheckedProjection::Deref { r#type } => current_type = Some(r#type.clone()),
            CheckedProjection::SliceLength => current_type = Some(ResolvedType::USize),
            CheckedProjection::EnumTag { r#type } => current_type = Some(r#type.clone()),
            CheckedProjection::EnumHeader { r#type, .. } => current_type = Some(r#type.clone()),
        }
    }
}
