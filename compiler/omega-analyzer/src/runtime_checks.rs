//! The four runtime invariants the compiler checks, and the resolved `core`
//! panic support the generated checks call.
//!
//! Analysis owns this because deciding *which* operations are checked and
//! resolving `core::panic` by name are both semantic questions; MIR only
//! consumes the resolved facts. See
//! [`docs/architecture/mir-and-codegen.md`](../../../../docs/architecture/mir-and-codegen.md).

use crate::checked::{
    CheckedAsmDescriptorKind, CheckedBlock, CheckedCoercion, CheckedCoercionStep, CheckedExpr,
    CheckedExprNode, CheckedMatchRemainder, CheckedPlace, CheckedPlaceRoot, CheckedRangeEnd,
    CheckedStmt, Storage,
};
use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_diagnostics::SourceFile;
use omega_hir::HirId;
use omega_parser::prelude::Span;
use std::rc::Rc;

/// A violated invariant a generated panic reports. The message is fixed: a
/// panic on this path formats nothing and allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCheck {
    EnumTagInMatch,
    EnumTagInWiden,
    EnumTagInTry,
    NeverCallReturned,
}

impl RuntimeCheck {
    pub fn message(self) -> &'static str {
        match self {
            Self::EnumTagInMatch => "invalid enum tag in match",
            Self::EnumTagInWiden => "invalid enum tag in anonymous-enum widening",
            Self::EnumTagInTry => "invalid enum tag in try operator",
            Self::NeverCallReturned => "never-returning call returned",
        }
    }
}

/// One resolved `core::panic::PanicInfo` field.
#[derive(Debug, Clone)]
pub struct PanicInfoField {
    pub index: usize,
    pub r#type: ResolvedType,
}

/// Everything a generated panic call needs, validated once per compilation
/// and shared by every function that emits one.
#[derive(Debug)]
pub struct PanicSupport {
    /// The gap function itself, by resolved declaration identity -- never by
    /// name, so a user function called `panic` is an ordinary call.
    pub handler_decl_id: HirId,
    pub handler_fn_type: ResolvedFunctionType,
    pub info_type: ResolvedType,
    pub source_file: PanicInfoField,
    pub line: PanicInfoField,
    pub column: PanicInfoField,
    pub message: PanicInfoField,
}

/// Where a generated panic reports it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanicLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// The panic support one function's generated checks use, together with the
/// source its spans index. A body's spans always belong to the file its text
/// was written in -- a macro expansion carries its call site's span, and a
/// generic instantiation keeps its template's -- so one file answers every
/// site in one body.
#[derive(Clone)]
pub struct FunctionRuntimeChecks {
    support: Rc<PanicSupport>,
    source: Rc<SourceFile>,
}

impl FunctionRuntimeChecks {
    pub fn new(support: Rc<PanicSupport>, source: Rc<SourceFile>) -> Self {
        Self { support, source }
    }

    pub fn support(&self) -> &PanicSupport {
        &self.support
    }

    pub fn location(&self, span: Span) -> PanicLocation {
        let (line, column) = self.source.line_col(span.start);
        PanicLocation {
            file: self.source.name().to_string(),
            line: line as u32,
            column: column as u32,
        }
    }
}

impl std::fmt::Debug for FunctionRuntimeChecks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionRuntimeChecks")
            .field("source", &self.source.name())
            .finish_non_exhaustive()
    }
}

/// The checked operations in one body that may need a generated panic.
///
/// A `never` call is a candidate even when it turns out to be the trusted
/// handler's own: telling the two apart needs the resolved gap declaration,
/// which MIR reads back off the attached support.
#[derive(Debug, Default)]
pub struct CandidateSites {
    pub tag_checks: usize,
    pub never_calls: usize,
}

impl CandidateSites {
    pub fn is_empty(&self) -> bool {
        self.tag_checks == 0 && self.never_calls == 0
    }
}

pub fn scan_block(block: &CheckedBlock) -> CandidateSites {
    let mut sites = CandidateSites::default();
    walk_block(block, &mut sites);
    sites
}

fn walk_block(block: &CheckedBlock, sites: &mut CandidateSites) {
    for stmt in &block.stmts {
        walk_stmt(stmt, sites);
    }
    if let Some(tail) = &block.tail {
        walk_expr(tail, sites);
    }
}

fn walk_stmt(stmt: &CheckedStmt, sites: &mut CandidateSites) {
    match stmt {
        CheckedStmt::Declaration(_) | CheckedStmt::Break(_) | CheckedStmt::Continue(_) => {}
        CheckedStmt::Expression(e) | CheckedStmt::Return(e) => walk_expr(e, sites),
        CheckedStmt::While(w) => {
            walk_expr(&w.condition, sites);
            walk_block(&w.body, sites);
        }
        CheckedStmt::Loop(l) => walk_block(&l.body, sites),
        CheckedStmt::For(f) => {
            for stmt in &f.init {
                walk_stmt(stmt, sites);
            }
            walk_expr(&f.condition, sites);
            if let Some(post) = &f.post {
                walk_expr(post, sites);
            }
            walk_block(&f.body, sites);
        }
        CheckedStmt::Defer(d) => walk_block(&d.body, sites),
        CheckedStmt::InlineAsm(asm) => {
            for descriptor in &asm.descriptors {
                if let CheckedAsmDescriptorKind::Reg { expr, .. } = &descriptor.kind {
                    walk_expr(expr, sites);
                }
            }
        }
    }
}

fn walk_expr(expr: &CheckedExprNode, sites: &mut CandidateSites) {
    match &expr.kind {
        CheckedExpr::Number(_)
        | CheckedExpr::Bool(_)
        | CheckedExpr::Char(_)
        | CheckedExpr::String(_)
        | CheckedExpr::ByteString(_)
        | CheckedExpr::Const(_)
        | CheckedExpr::Sizeof(_)
        | CheckedExpr::Alignof(_) => {}
        CheckedExpr::Place(place) => walk_place(place, sites),
        CheckedExpr::FunctionCall(call) => {
            walk_expr(&call.callee, sites);
            for arg in &call.args {
                walk_expr(arg, sites);
            }
            if *call.fn_type.return_type == ResolvedType::Never {
                sites.never_calls += 1;
            }
        }
        CheckedExpr::DynamicCall(call) => {
            walk_place(&call.base, sites);
            for arg in &call.args {
                walk_expr(arg, sites);
            }
            if *call.fn_type.return_type == ResolvedType::Never {
                sites.never_calls += 1;
            }
        }
        CheckedExpr::Assignment(a) => {
            walk_place(&a.target, sites);
            walk_expr(&a.value, sites);
        }
        CheckedExpr::CompoundAssign(a) => {
            walk_place(&a.place, sites);
            walk_expr(&a.value, sites);
        }
        CheckedExpr::AddressOf(a) => walk_place(&a.place, sites),
        CheckedExpr::Negate(e) | CheckedExpr::BitNot(e) => walk_expr(e, sites),
        CheckedExpr::BinaryOp(b) => {
            walk_expr(&b.left, sites);
            walk_expr(&b.right, sites);
        }
        CheckedExpr::Codeblock(block) => walk_block(block, sites),
        CheckedExpr::If(if_expr) => {
            for (condition, block) in &if_expr.branches {
                walk_expr(condition, sites);
                walk_block(block, sites);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                walk_block(else_branch, sites);
            }
        }
        CheckedExpr::ArrayLiteral(literal) => {
            for element in &literal.elements {
                walk_expr(element, sites);
            }
        }
        CheckedExpr::StructLiteral(literal) => {
            for field in &literal.fields {
                walk_expr(&field.value, sites);
            }
        }
        CheckedExpr::EnumConstruct(construct) => {
            for field in &construct.fields {
                walk_expr(&field.value, sites);
            }
        }
        CheckedExpr::UnionConstruct(construct) => walk_expr(&construct.value, sites),
        CheckedExpr::Slice(slice) => {
            walk_place(&slice.base, sites);
            if let Some(start) = &slice.start {
                walk_expr(start, sites);
            }
            match &slice.end {
                CheckedRangeEnd::Inclusive(end) | CheckedRangeEnd::Exclusive(end) => {
                    walk_expr(end, sites)
                }
                CheckedRangeEnd::Open => {}
            }
        }
        CheckedExpr::Match(m) => {
            for arm in &m.arms {
                for group in &arm.conditions {
                    for condition in group {
                        walk_expr(condition, sites);
                    }
                }
                walk_block(&arm.body, sites);
            }
            match &m.else_branch {
                Some(else_branch) => walk_block(else_branch, sites),
                None if m.remainder == CheckedMatchRemainder::IllegalEnumTag => {
                    sites.tag_checks += 1;
                }
                None => {}
            }
        }
        CheckedExpr::Cast(cast) => walk_expr(&cast.base, sites),
        CheckedExpr::SpecCoerce(coerce) => walk_expr(&coerce.base, sites),
        CheckedExpr::AnonymousEnumWiden(widen) => {
            walk_expr(&widen.source, sites);
            sites.tag_checks += 1;
        }
        CheckedExpr::Try(r#try) => {
            walk_expr(&r#try.operand, sites);
            sites.tag_checks += 1;
            walk_coercion(&r#try.destination.error_coercion, sites);
        }
    }
}

fn walk_coercion(coercion: &CheckedCoercion, sites: &mut CandidateSites) {
    for step in &coercion.steps {
        if let CheckedCoercionStep::WidenAnonymousEnum { .. } = step {
            sites.tag_checks += 1;
        }
    }
}

fn walk_place(place: &CheckedPlace, sites: &mut CandidateSites) {
    if let CheckedPlaceRoot::Expr(expr) = &place.root {
        walk_expr(expr, sites);
    }
    for projection in &place.projections {
        if let crate::checked::CheckedProjection::Index { index_expr, .. } = projection {
            walk_expr(index_expr, sites);
        }
    }
}

/// The declaration a call names directly, when it names one at all. A call
/// through a function pointer or a stored value has no static callee and is
/// therefore never the trusted handler.
pub fn static_callee(callee: &CheckedExprNode) -> Option<HirId> {
    let CheckedExpr::Place(CheckedPlace {
        root:
            CheckedPlaceRoot::Variable {
                decl_id,
                storage: Storage::Function,
                ..
            },
        projections,
        ..
    }) = &callee.kind
    else {
        return None;
    };
    projections.is_empty().then_some(*decl_id)
}
