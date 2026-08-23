use crate::checked::{
    CheckedAsmDescriptorKind, CheckedBlock, CheckedExpr, CheckedExprNode, CheckedItem,
    CheckedModule, CheckedPlace, CheckedPlaceRoot, CheckedProjection, CheckedRangeEnd, CheckedStmt,
};
use crate::resolved_type::ResolvedType;
use omega_hir::HirId;
use std::collections::HashSet;

#[derive(Default)]
pub struct FieldUsage {
    pub struct_fields: HashSet<(HirId, usize)>,
    pub union_fields: HashSet<(HirId, usize)>,
    pub enum_dynamic_fields: HashSet<(HirId, usize)>,
    pub enum_body_fields: HashSet<(HirId, usize, usize)>,
    pub enum_variants: HashSet<(HirId, usize)>,
}

impl FieldUsage {
    pub fn merge(&mut self, other: FieldUsage) {
        self.struct_fields.extend(other.struct_fields);
        self.union_fields.extend(other.union_fields);
        self.enum_dynamic_fields.extend(other.enum_dynamic_fields);
        self.enum_body_fields.extend(other.enum_body_fields);
        self.enum_variants.extend(other.enum_variants);
    }
}

pub fn collect_module(module: &CheckedModule, usage: &mut FieldUsage) {
    for item in &module.items {
        collect_item(item, usage);
    }
}

fn collect_item(item: &CheckedItem, usage: &mut FieldUsage) {
    match item {
        CheckedItem::Declaration(_) | CheckedItem::ForeignBinding(_) => {}
        CheckedItem::ForeignFunction(f) => {
            if let Some(body) = &f.body {
                collect_block(body, usage);
            }
        }
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
        CheckedStmt::Declaration(_) | CheckedStmt::Break(_) | CheckedStmt::Continue(_) => {}
        CheckedStmt::Expression(e) | CheckedStmt::Return(e) => collect_expr(e, usage),
        CheckedStmt::While(w) => {
            collect_expr(&w.condition, usage);
            collect_block(&w.body, usage);
        }
        CheckedStmt::Loop(l) => collect_block(&l.body, usage),
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
        CheckedStmt::InlineAsm(asm) => {
            for descriptor in &asm.descriptors {
                if let CheckedAsmDescriptorKind::Reg { expr, .. } = &descriptor.kind {
                    collect_expr(expr, usage);
                }
            }
        }
    }
}

pub(crate) fn collect_expr(expr: &CheckedExprNode, usage: &mut FieldUsage) {
    match &expr.kind {
        CheckedExpr::Number(_)
        | CheckedExpr::Bool(_)
        | CheckedExpr::Char(_)
        | CheckedExpr::String(_)
        | CheckedExpr::ByteString(_)
        | CheckedExpr::Const(_)
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
        CheckedExpr::CompoundAssign(a) => {
            collect_place(&a.place, usage);
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
                usage
                    .enum_variants
                    .insert((cell.borrow().id, construct.variant_index));
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
            match &s.end {
                CheckedRangeEnd::Inclusive(end) => collect_expr(end, usage),
                CheckedRangeEnd::Exclusive(end) => collect_expr(end, usage),
                _ => {}
            }
        }
        CheckedExpr::Match(m) => {
            for arm in &m.arms {
                for group in &arm.conditions {
                    for cond in group {
                        collect_expr(cond, usage);
                    }
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
        CheckedExpr::AnonymousEnumWiden(widen) => collect_expr(&widen.source, usage),
        CheckedExpr::DynamicCall(call) => {
            collect_place(&call.base, usage);
            for arg in &call.args {
                collect_expr(arg, usage);
            }
        }
    }
}

pub(crate) fn collect_place(place: &CheckedPlace, usage: &mut FieldUsage) {
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
            CheckedProjection::EnumBody {
                variant_index,
                field_index,
                r#type,
                ..
            } => {
                if let Some(ResolvedType::Enum { cell, .. }) = &current_type {
                    usage
                        .enum_body_fields
                        .insert((cell.borrow().id, *variant_index, *field_index));
                }
                current_type = Some(r#type.clone());
            }
            CheckedProjection::Index {
                index_expr,
                item_type,
            } => {
                collect_expr(index_expr, usage);
                current_type = Some(item_type.clone());
            }
            CheckedProjection::Deref { r#type } => current_type = Some(r#type.clone()),
            CheckedProjection::SliceLength => current_type = Some(ResolvedType::USize),
            CheckedProjection::SpecObjectPtr { mutable } => {
                current_type = Some(ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::U8),
                    mutable: *mutable,
                })
            }
            CheckedProjection::SpecObjectVtable => {
                current_type = Some(ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::U8),
                    mutable: false,
                })
            }
            CheckedProjection::EnumTag { r#type } => current_type = Some(r#type.clone()),
            CheckedProjection::EnumHeader { r#type, .. } => current_type = Some(r#type.clone()),
        }
    }
}
