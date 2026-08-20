mod control_flow;
mod defer;
mod expr;

use super::place::place_align;
use crate::body::{
    MirAssignment, MirBlockData, MirBody, MirExpr, MirExprNode, MirLocalDecl, MirPlace,
    MirPlaceRoot, MirTerminator,
};
use crate::ids::{BlockId, LocalId};
use omega_analyzer::checked::{
    CheckedAssignment, CheckedBlock, CheckedBreak, CheckedContinue, CheckedDefer, CheckedExpr,
    CheckedExprNode, CheckedFor, CheckedLoop, CheckedParam, CheckedPlace, CheckedStmt,
    CheckedWhile,
};
use omega_analyzer::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::Span;
use std::collections::HashMap;

#[derive(Default)]
struct BuilderBlock {
    statements: Vec<MirExprNode>,
    terminator: Option<MirTerminator>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: BlockId,
    continue_block: BlockId,
}

#[derive(Clone, Copy)]
struct LoopScope {
    id: HirId,
    targets: LoopTargets,
}

#[derive(Clone, Copy)]
struct BlockDestination {
    merge: BlockId,
    result: Option<LocalId>,
}

impl BlockDestination {
    fn new(merge: BlockId, result: Option<LocalId>) -> Self {
        Self { merge, result }
    }
}

#[derive(Clone, Copy)]
struct DeferSite {
    id: HirId,
    span: Span,
    flag: LocalId,
    check_block: BlockId,
    run_block: BlockId,
}

pub(crate) struct FunctionLowerer {
    locals: Vec<MirLocalDecl>,
    local_of: HashMap<HirId, LocalId>,
    blocks: Vec<BuilderBlock>,
    current: BlockId,
    loop_stack: Vec<LoopScope>,
    return_slot: Option<LocalId>,
    exit_chain_start: BlockId,
    defer_bodies: HashMap<HirId, CheckedBlock>,
    defer_flag_of: HashMap<HirId, LocalId>,
}

impl FunctionLowerer {
    pub(crate) fn lower(
        params: &[CheckedParam],
        body: CheckedBlock,
        return_type: &ResolvedType,
        fn_id: HirId,
        fn_span: Span,
    ) -> MirBody {
        let mut lowerer = Self::new();

        for param in params {
            lowerer.declare_local(Some(param.id), param.r#type.clone());
        }
        let arg_count = lowerer.locals.len();

        let entry = lowerer.current;
        let defers = defer::collect_defer_ids(&body)
            .into_iter()
            .map(|(id, span)| {
                let site = DeferSite {
                    id,
                    span,
                    flag: lowerer.declare_local(None, ResolvedType::Bool),
                    check_block: lowerer.new_block(),
                    run_block: lowerer.new_block(),
                };
                let previous = lowerer.defer_flag_of.insert(id, site.flag);
                assert!(
                    previous.is_none(),
                    "omega-mir lowering bug: duplicate defer HirId in one function"
                );
                site
            })
            .collect::<Vec<_>>();

        lowerer.return_slot = (!matches!(return_type, ResolvedType::Void))
            .then(|| lowerer.declare_local(None, return_type.clone()));

        let final_block = lowerer.new_block();
        lowerer.exit_chain_start = defers
            .last()
            .map(|site| site.check_block)
            .unwrap_or(final_block);

        lowerer.current = entry;
        for site in &defers {
            lowerer.assign_local(
                site.id,
                site.span,
                site.flag,
                bool_literal(site.id, site.span, false),
            );
        }

        if lowerer.lower_function_body(body) {
            lowerer.terminate(MirTerminator::Goto(lowerer.exit_chain_start));
        }

        let mut next_block = final_block;
        for site in &defers {
            lowerer.current = site.check_block;
            let condition = lowerer.local_expr(site.flag, site.id, site.span);
            lowerer.terminate(MirTerminator::Branch {
                condition,
                then_block: site.run_block,
                else_block: next_block,
            });

            lowerer.current = site.run_block;
            let defer_body = lowerer
                .defer_bodies
                .remove(&site.id)
                .expect("every collected defer must be lowered and registered");
            let fell_through = lowerer.lower_block_as_stmt(defer_body);
            if fell_through {
                lowerer.terminate(MirTerminator::Goto(next_block));
            }
            next_block = site.check_block;
        }
        assert_eq!(
            next_block, lowerer.exit_chain_start,
            "omega-mir lowering bug: defer exit chain was assembled out of order"
        );

        lowerer.current = final_block;
        let return_value = lowerer
            .return_slot
            .map(|slot| lowerer.local_expr(slot, fn_id, fn_span));
        lowerer.terminate(MirTerminator::Return(return_value));

        lowerer.finish(arg_count)
    }

    fn new() -> Self {
        Self {
            locals: Vec::new(),
            local_of: HashMap::new(),
            blocks: vec![BuilderBlock::default()],
            current: BlockId::from_index(0),
            loop_stack: Vec::new(),
            return_slot: None,
            exit_chain_start: BlockId::from_index(0),
            defer_bodies: HashMap::new(),
            defer_flag_of: HashMap::new(),
        }
    }

    fn declare_local(&mut self, source: Option<HirId>, r#type: ResolvedType) -> LocalId {
        let id = LocalId::from_index(self.locals.len());
        if let Some(hir_id) = source {
            let previous = self.local_of.insert(hir_id, id);
            assert!(
                previous.is_none(),
                "omega-mir lowering bug: one HIR declaration was assigned two MIR locals"
            );
        }
        self.locals.push(MirLocalDecl { source, r#type });
        id
    }

    fn local_type(&self, local: LocalId) -> &ResolvedType {
        &self.locals[local.index()].r#type
    }

    fn local_place(&self, local: LocalId) -> MirPlace {
        let r#type = self.local_type(local).clone();
        MirPlace {
            root: MirPlaceRoot::Local {
                id: local,
                r#type: r#type.clone(),
            },
            projections: Vec::new(),
            align: place_align(&r#type),
            r#type,
        }
    }

    fn local_expr(&self, local: LocalId, id: HirId, span: Span) -> MirExprNode {
        let place = self.local_place(local);
        MirExprNode {
            id,
            span,
            r#type: place.r#type.clone(),
            kind: MirExpr::Place(place),
        }
    }

    pub(super) fn local_for_hir(&self, hir_id: HirId) -> LocalId {
        *self.local_of.get(&hir_id).unwrap_or_else(|| {
            panic!("checked module guarantees {hir_id:?} was declared before this use")
        })
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId::from_index(self.blocks.len());
        self.blocks.push(BuilderBlock::default());
        id
    }

    fn current_block(&self) -> &BuilderBlock {
        &self.blocks[self.current.index()]
    }

    fn current_block_mut(&mut self) -> &mut BuilderBlock {
        &mut self.blocks[self.current.index()]
    }

    fn is_current_terminated(&self) -> bool {
        self.current_block().terminator.is_some()
    }

    fn push_stmt(&mut self, expr: MirExprNode) {
        assert!(
            !self.is_current_terminated(),
            "omega-mir lowering bug: cannot append a statement to a terminated block"
        );
        self.current_block_mut().statements.push(expr);
    }

    fn terminate(&mut self, terminator: MirTerminator) {
        assert!(
            !self.is_current_terminated(),
            "omega-mir lowering bug: a block can only be terminated once"
        );
        self.current_block_mut().terminator = Some(terminator);
    }

    /// Stores `value` into a fresh local and returns an expression reading
    /// it back, so the caller can use the result more than once without
    /// re-executing `value`.
    pub(super) fn materialize_once(&mut self, value: MirExprNode) -> MirExprNode {
        let id = value.id;
        let span = value.span;
        let local = self.declare_local(None, value.r#type.clone());
        self.assign_local(id, span, local, value);
        self.local_expr(local, id, span)
    }

    fn assign_local(&mut self, id: HirId, span: Span, local: LocalId, value: MirExprNode) {
        let target = self.local_place(local);
        self.push_stmt(MirExprNode {
            id,
            span,
            r#type: ResolvedType::Void,
            kind: MirExpr::Assignment(MirAssignment {
                target,
                value: Box::new(value),
            }),
        });
    }

    fn loop_target(&self, loop_id: HirId) -> LoopTargets {
        self.loop_stack
            .iter()
            .rev()
            .find(|scope| scope.id == loop_id)
            .map(|scope| scope.targets)
            .expect("checked module guarantees break/continue targets a currently-enclosing loop")
    }

    fn push_loop(&mut self, id: HirId, targets: LoopTargets) {
        self.loop_stack.push(LoopScope { id, targets });
    }

    fn pop_loop(&mut self, id: HirId) {
        let scope = self
            .loop_stack
            .pop()
            .expect("omega-mir lowering bug: loop stack unexpectedly empty");
        assert_eq!(
            scope.id, id,
            "omega-mir lowering bug: loop scopes were popped out of order"
        );
    }

    fn finish(self, arg_count: usize) -> MirBody {
        assert!(
            arg_count <= self.locals.len(),
            "omega-mir lowering bug: argument count exceeds local count"
        );
        assert!(
            self.loop_stack.is_empty(),
            "omega-mir lowering bug: loop scope leaked past function lowering"
        );
        assert!(
            self.defer_bodies.is_empty(),
            "omega-mir lowering bug: a defer body was registered but never emitted"
        );

        let blocks = self
            .blocks
            .into_iter()
            .enumerate()
            .map(|(index, block)| MirBlockData {
                statements: block.statements,
                terminator: block.terminator.unwrap_or_else(|| {
                    panic!("omega-mir lowering bug: block {index} was never terminated")
                }),
            })
            .collect();
        MirBody {
            locals: self.locals,
            arg_count,
            blocks,
        }
    }

    fn lower_stmts(&mut self, stmts: Vec<CheckedStmt>) {
        for stmt in stmts {
            if self.is_current_terminated() {
                break;
            }
            self.lower_stmt(stmt);
        }
    }

    fn lower_stmt(&mut self, stmt: CheckedStmt) {
        match stmt {
            CheckedStmt::Declaration(decl) => {
                self.declare_local(Some(decl.id), decl.r#type);
            }
            CheckedStmt::ExternDeclaration(_) => {
                todo!("extern declarations inside a function body are not yet implemented");
            }
            CheckedStmt::Expression(expr) => self.lower_expr_stmt(expr),
            CheckedStmt::Return(expr) => self.lower_return(expr),
            CheckedStmt::While(CheckedWhile {
                id,
                condition,
                body,
                ..
            }) => self.lower_while(id, condition, body),
            CheckedStmt::Loop(CheckedLoop {
                id,
                body,
                has_break,
                ..
            }) => self.lower_loop(id, body, has_break),
            CheckedStmt::For(for_loop) => {
                let CheckedFor {
                    id,
                    init,
                    condition,
                    post,
                    body,
                    ..
                } = *for_loop;
                self.lower_for(id, init, condition, post, body);
            }
            CheckedStmt::Break(CheckedBreak { loop_id, .. }) => {
                let target = self.loop_target(loop_id).break_block;
                self.terminate(MirTerminator::Goto(target));
            }
            CheckedStmt::Continue(CheckedContinue { loop_id, .. }) => {
                let target = self.loop_target(loop_id).continue_block;
                self.terminate(MirTerminator::Goto(target));
            }
            CheckedStmt::Defer(CheckedDefer { id, span, body }) => {
                let flag = *self
                    .defer_flag_of
                    .get(&id)
                    .expect("every checked defer has a flag allocated by the MIR defer pre-pass");
                self.assign_local(id, span, flag, bool_literal(id, span, true));
                let previous = self.defer_bodies.insert(id, body);
                assert!(
                    previous.is_none(),
                    "omega-mir lowering bug: duplicate defer HirId in one function"
                );
            }
        }
    }

    fn lower_return(&mut self, expr: CheckedExprNode) {
        if is_control_flow_expr(&expr.kind) {
            self.lower_control_flow_into(
                expr,
                BlockDestination::new(self.exit_chain_start, self.return_slot),
            );
            return;
        }

        let diverges = expr.r#type == ResolvedType::Never;
        let value = self.lower_expr(expr);
        if self.is_current_terminated() {
            return;
        }
        if diverges {
            self.push_stmt(value);
            self.terminate(MirTerminator::Unreachable);
            return;
        }
        match self.return_slot {
            Some(slot) => self.assign_local(value.id, value.span, slot, value),
            None => self.push_stmt(value),
        }
        self.terminate(MirTerminator::Goto(self.exit_chain_start));
    }

    fn lower_expr_stmt(&mut self, expr: CheckedExprNode) {
        let CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        } = expr;

        if let CheckedExpr::Assignment(assignment) = kind {
            self.lower_assignment_stmt(id, span, r#type, assignment);
            return;
        }

        let expr = CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        };
        if is_control_flow_expr(&expr.kind) {
            self.lower_control_flow_stmt(expr, None);
        } else {
            self.lower_plain_expr_stmt(expr);
        }
    }

    fn lower_assignment_stmt(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        assignment: CheckedAssignment,
    ) {
        let target = self.lower_place(assignment.target);
        if is_control_flow_expr(&assignment.value.kind)
            && let Some(local) = bare_local(&target)
        {
            self.lower_control_flow_stmt(*assignment.value, Some(local));
            return;
        }

        let diverges = assignment.value.r#type == ResolvedType::Never;
        let value = Box::new(self.lower_expr(*assignment.value));
        if self.is_current_terminated() {
            return;
        }
        self.push_stmt(MirExprNode {
            id,
            span,
            r#type,
            kind: MirExpr::Assignment(MirAssignment { target, value }),
        });
        if diverges {
            self.terminate(MirTerminator::Unreachable);
        }
    }

    fn lower_plain_expr_stmt(&mut self, expr: CheckedExprNode) -> bool {
        let diverges = expr.r#type == ResolvedType::Never;
        let node = self.lower_expr(expr);
        if self.is_current_terminated() {
            return false;
        }
        self.push_stmt(node);
        if diverges {
            self.terminate(MirTerminator::Unreachable);
            false
        } else {
            true
        }
    }

    fn lower_function_body(&mut self, block: CheckedBlock) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        let Some(tail) = block.tail else {
            return true;
        };

        if is_control_flow_expr(&tail.kind) {
            self.lower_control_flow_into(
                *tail,
                BlockDestination::new(self.exit_chain_start, self.return_slot),
            );
            return false;
        }

        let diverges = tail.r#type == ResolvedType::Never;
        let value = self.lower_expr(*tail);
        if self.is_current_terminated() {
            return false;
        }
        if diverges {
            self.push_stmt(value);
            self.terminate(MirTerminator::Unreachable);
            return false;
        }
        match self.return_slot {
            Some(slot) => self.assign_local(value.id, value.span, slot, value),
            None => self.push_stmt(value),
        }
        true
    }

    fn lower_block_as_stmt(&mut self, block: CheckedBlock) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        let Some(tail) = block.tail else {
            return true;
        };

        if is_control_flow_expr(&tail.kind) {
            self.lower_control_flow_stmt(*tail, None);
            !self.is_current_terminated()
        } else {
            self.lower_plain_expr_stmt(*tail)
        }
    }

    pub(super) fn lower_expr(&mut self, node: CheckedExprNode) -> MirExprNode {
        expr::lower_expr(self, node)
    }

    fn lower_place(&mut self, place: CheckedPlace) -> MirPlace {
        super::place::lower_place(self, place)
    }

    pub(super) fn lower_place_evaluated_once(&mut self, place: CheckedPlace) -> MirPlace {
        super::place::lower_place_evaluated_once(self, place)
    }
}

fn bool_literal(id: HirId, span: Span, value: bool) -> MirExprNode {
    MirExprNode {
        id,
        span,
        r#type: ResolvedType::Bool,
        kind: MirExpr::Bool(value),
    }
}

fn bare_local(place: &MirPlace) -> Option<LocalId> {
    if !place.projections.is_empty() {
        return None;
    }
    match &place.root {
        MirPlaceRoot::Local { id, .. } => Some(*id),
        _ => None,
    }
}

fn is_control_flow_expr(kind: &CheckedExpr) -> bool {
    matches!(
        kind,
        CheckedExpr::If(_) | CheckedExpr::Match(_) | CheckedExpr::Codeblock(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_analyzer::checked::{CheckedFunctionCall, CheckedPlaceRoot, Storage};
    use omega_analyzer::resolved_type::ResolvedFunctionType;
    use omega_hir::ModuleId;

    fn hir_id(local: u32) -> HirId {
        HirId {
            module: ModuleId(0),
            local,
        }
    }

    fn void_function_type() -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: Vec::new(),
            return_type: Box::new(ResolvedType::Void),
            is_variadic: false,
            self_mode: None,
        }
    }

    fn void_call(id: u32) -> CheckedExprNode {
        let fn_type = void_function_type();
        let callee_type = ResolvedType::Function(fn_type.clone());
        let callee = CheckedExprNode {
            id: hir_id(id + 1),
            span: Span::default(),
            r#type: callee_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable {
                    decl_id: hir_id(id + 2),
                    storage: Storage::Function,
                    r#type: callee_type.clone(),
                },
                projections: Vec::new(),
                r#type: callee_type,
            }),
        };

        CheckedExprNode {
            id: hir_id(id),
            span: Span::default(),
            r#type: ResolvedType::Void,
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(callee),
                fn_type,
                args: Vec::new(),
            }),
        }
    }

    fn lower_void_body(body: CheckedBlock) -> MirBody {
        FunctionLowerer::lower(&[], body, &ResolvedType::Void, hir_id(100), Span::default())
    }

    fn assert_entry_emits_call(body: &MirBody) {
        assert!(matches!(
            body.blocks[0].statements.first().map(|node| &node.kind),
            Some(MirExpr::FunctionCall(_))
        ));
    }

    #[test]
    fn void_return_expression_is_emitted_for_effects() {
        let body = lower_void_body(CheckedBlock {
            stmts: vec![CheckedStmt::Return(void_call(1))],
            tail: None,
        });

        assert_entry_emits_call(&body);
    }

    #[test]
    fn void_function_tail_is_emitted_for_effects() {
        let body = lower_void_body(CheckedBlock {
            stmts: Vec::new(),
            tail: Some(Box::new(void_call(1))),
        });

        assert_entry_emits_call(&body);
    }

    #[test]
    fn non_breaking_loop_has_no_fallthrough_block() {
        let body = lower_void_body(CheckedBlock {
            stmts: vec![CheckedStmt::Loop(non_breaking_loop(1))],
            tail: None,
        });

        assert!(
            body.blocks
                .iter()
                .any(|block| matches!(&block.terminator, MirTerminator::Unreachable))
        );
    }

    #[test]
    fn defer_body_may_diverge_without_corrupting_the_cfg() {
        let body = lower_void_body(CheckedBlock {
            stmts: vec![CheckedStmt::Defer(CheckedDefer {
                id: hir_id(1),
                span: Span::default(),
                body: CheckedBlock {
                    stmts: vec![CheckedStmt::Loop(non_breaking_loop(2))],
                    tail: None,
                },
            })],
            tail: None,
        });

        assert!(
            body.blocks
                .iter()
                .any(|block| matches!(&block.terminator, MirTerminator::Unreachable))
        );
    }

    fn non_breaking_loop(local: u32) -> CheckedLoop {
        CheckedLoop {
            id: hir_id(local),
            span: Span::default(),
            body: CheckedBlock {
                stmts: Vec::new(),
                tail: None,
            },
            has_break: false,
        }
    }
}
