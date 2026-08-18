use super::place::place_align;
use crate::body::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirBlockData, MirBody, MirCast,
    MirDynamicCall, MirEnumConstruct, MirExpr, MirExprNode, MirFieldInit, MirFunctionCall,
    MirLocalDecl, MirPlace, MirPlaceRoot, MirSlice, MirSpecCoerce, MirStructLiteral, MirTerminator,
    MirUnionConstruct,
};
use crate::ids::{BlockId, LocalId};
use omega_analyzer::checked::{
    CheckedBlock, CheckedBreak, CheckedContinue, CheckedDefer, CheckedExpr, CheckedExprNode,
    CheckedFor, CheckedIf, CheckedLoop, CheckedMatch, CheckedMatchArm, CheckedParam, CheckedPlace,
    CheckedPlaceRoot, CheckedProjection, CheckedRangeEnd, CheckedStmt, CheckedStructLiteralField,
    CheckedWhile,
};
use omega_analyzer::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::Span;
use std::collections::HashMap;

struct BuilderBlock {
    statements: Vec<MirExprNode>,
    terminator: Option<MirTerminator>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: BlockId,
    continue_block: BlockId,
}

pub(crate) struct FunctionLowerer {
    locals: Vec<MirLocalDecl>,
    pub(super) local_of: HashMap<HirId, LocalId>,
    blocks: Vec<BuilderBlock>,
    current: BlockId,
    loop_stack: Vec<(HirId, LoopTargets)>,
    return_slot: Option<LocalId>,
    return_type: ResolvedType,
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
        let mut lowerer = FunctionLowerer {
            locals: Vec::new(),
            local_of: HashMap::new(),
            blocks: Vec::new(),
            current: BlockId(0),
            loop_stack: Vec::new(),
            return_slot: None,
            return_type: return_type.clone(),
            exit_chain_start: BlockId(0),
            defer_bodies: HashMap::new(),
            defer_flag_of: HashMap::new(),
        };

        for param in params {
            lowerer.declare_local(Some(param.id), param.r#type.clone());
        }
        let arg_count = lowerer.locals.len();

        // Create the entry block first so it is always BlockId(0).
        let entry = lowerer.new_block();

        // Collect all defers before lowering because nested returns need the complete exit chain up front.
        let mut defers = Vec::new();
        collect_defer_ids(&body, &mut defers);
        let defer_flags: Vec<LocalId> = defers
            .iter()
            .map(|_| lowerer.declare_local(None, ResolvedType::Bool))
            .collect();
        for (&flag, (id, _)) in defer_flags.iter().zip(&defers) {
            lowerer.defer_flag_of.insert(*id, flag);
        }

        lowerer.return_slot = (!matches!(return_type, ResolvedType::Void))
            .then(|| lowerer.declare_local(None, return_type.clone()));

        // Reserve defer check/run blocks before body lowering so return targets are stable.
        let check_blocks: Vec<BlockId> = defers.iter().map(|_| lowerer.new_block()).collect();
        let run_blocks: Vec<BlockId> = defers.iter().map(|_| lowerer.new_block()).collect();
        let final_block = lowerer.new_block();
        // Store defer blocks in declaration order; execution walks them in reverse.
        lowerer.exit_chain_start = check_blocks.last().copied().unwrap_or(final_block);

        lowerer.current = entry;

        // Each defer gets a flag that records whether control reached its declaration.
        for (&flag, (id, span)) in defer_flags.iter().zip(&defers) {
            lowerer.assign_local(
                *id,
                *span,
                flag,
                ResolvedType::Bool,
                bool_literal(*id, *span, false),
            );
        }

        // Lower the body through the value-producing path because analysis already proved its tail/return shape.
        let fell_through = lowerer.lower_function_body(body);
        if fell_through {
            lowerer.set_terminator(MirTerminator::Goto(lowerer.exit_chain_start));
        }

        // Emit deferred bodies only after the main body has populated the defer table.
        for (i, (id, span)) in defers.iter().enumerate().rev() {
            let check = check_blocks[i];
            let run = run_blocks[i];
            // Run active defers in reverse declaration order.
            let next = if i == 0 {
                final_block
            } else {
                check_blocks[i - 1]
            };
            let flag = defer_flags[i];

            lowerer.current = check;
            let flag_read = MirExprNode {
                id: *id,
                span: *span,
                r#type: ResolvedType::Bool,
                kind: MirExpr::Place(MirPlace {
                    root: MirPlaceRoot::Local {
                        id: flag,
                        r#type: ResolvedType::Bool,
                    },
                    projections: vec![],
                    r#type: ResolvedType::Bool,
                    align: place_align(&ResolvedType::Bool),
                }),
            };
            lowerer.set_terminator(MirTerminator::Branch {
                condition: flag_read,
                then_block: run,
                else_block: next,
            });

            lowerer.current = run;
            let defer_body = lowerer
                .defer_bodies
                .remove(id)
                .expect("every collected defer is visited unconditionally during the walk above");
            let fell_through = lowerer.lower_block_as_stmt(defer_body);
            assert!(
                fell_through,
                "a defer body can never diverge -- analysis rejects return/break/continue inside one"
            );
            lowerer.set_terminator(MirTerminator::Goto(next));
        }

        lowerer.current = final_block;
        let return_value = lowerer.return_slot.map(|slot| MirExprNode {
            id: fn_id,
            span: fn_span,
            r#type: return_type.clone(),
            kind: MirExpr::Place(MirPlace {
                root: MirPlaceRoot::Local {
                    id: slot,
                    r#type: return_type.clone(),
                },
                projections: vec![],
                r#type: return_type.clone(),
                align: place_align(&return_type),
            }),
        });
        lowerer.set_terminator(MirTerminator::Return(return_value));

        lowerer.finish(arg_count)
    }

    fn declare_local(&mut self, source: Option<HirId>, r#type: ResolvedType) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        if let Some(hir_id) = source {
            self.local_of.insert(hir_id, id);
        }
        self.locals.push(MirLocalDecl { source, r#type });
        id
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BuilderBlock {
            statements: Vec::new(),
            terminator: None,
        });
        id
    }

    fn is_current_terminated(&self) -> bool {
        self.blocks[self.current.0 as usize].terminator.is_some()
    }

    fn push_stmt(&mut self, expr: MirExprNode) {
        debug_assert!(
            !self.is_current_terminated(),
            "cannot append a statement to an already-terminated block"
        );
        self.blocks[self.current.0 as usize].statements.push(expr);
    }

    fn set_terminator(&mut self, terminator: MirTerminator) {
        debug_assert!(
            !self.is_current_terminated(),
            "a block can only be terminated once"
        );
        self.blocks[self.current.0 as usize].terminator = Some(terminator);
    }

    fn assign_local(
        &mut self,
        id: HirId,
        span: Span,
        local: LocalId,
        r#type: ResolvedType,
        value: MirExprNode,
    ) {
        let target = MirPlace {
            root: MirPlaceRoot::Local {
                id: local,
                r#type: r#type.clone(),
            },
            projections: vec![],
            r#type: r#type.clone(),
            align: place_align(&r#type),
        };
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
            .find(|(id, _)| *id == loop_id)
            .map(|(_, targets)| *targets)
            .expect("checked module guarantees a break/continue's loop_id is a currently-enclosing loop")
    }

    fn finish(self, arg_count: usize) -> MirBody {
        let blocks = self
            .blocks
            .into_iter()
            .enumerate()
            .map(|(i, b)| MirBlockData {
                statements: b.statements,
                terminator: b.terminator.unwrap_or_else(|| {
                    panic!("omega-mir lowering bug: block {i} was never terminated")
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
            CheckedStmt::Return(expr) => {
                // Lower value-producing control flow directly into the return destination to avoid a temporary.
                if is_control_flow_expr(&expr.kind) {
                    let result = self.return_slot;
                    let result_type = self.return_type.clone();
                    self.lower_control_flow_into(expr, self.exit_chain_start, result, result_type);
                    return;
                }
                let diverges = expr.r#type == ResolvedType::Never;
                let value = self.lower_expr(expr);
                if self.is_current_terminated() {
                    return;
                }
                if let Some(slot) = self.return_slot {
                    let id = value.id;
                    let span = value.span;
                    let r#type = value.r#type.clone();
                    self.assign_local(id, span, slot, r#type, value);
                }
                // A never-returning expression terminates control flow and must not synthesize a return value.
                self.set_terminator(if diverges {
                    MirTerminator::Unreachable
                } else {
                    MirTerminator::Goto(self.exit_chain_start)
                });
            }
            CheckedStmt::While(CheckedWhile {
                id,
                condition,
                body,
                ..
            }) => {
                self.lower_while(id, condition, body);
            }
            CheckedStmt::Loop(CheckedLoop { id, body, .. }) => {
                self.lower_loop(id, body);
            }
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
                self.set_terminator(MirTerminator::Goto(target));
            }
            CheckedStmt::Continue(CheckedContinue { loop_id, .. }) => {
                let target = self.loop_target(loop_id).continue_block;
                self.set_terminator(MirTerminator::Goto(target));
            }
            CheckedStmt::Defer(CheckedDefer { id, span, body }) => {
                let flag = *self
                    .defer_flag_of
                    .get(&id)
                    .expect("every defer's flag local is pre-allocated by the pre-pass in `lower`");
                self.assign_local(
                    id,
                    span,
                    flag,
                    ResolvedType::Bool,
                    bool_literal(id, span, true),
                );
                self.defer_bodies.insert(id, body);
            }
        }
    }

    fn lower_expr_stmt(&mut self, expr: CheckedExprNode) {
        let CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        } = expr;

        if let CheckedExpr::Assignment(assignment) = kind {
            if is_control_flow_expr(&assignment.value.kind) {
                let target = self.lower_place(assignment.target);
                if target.projections.is_empty()
                    && let MirPlaceRoot::Local {
                        id: local_id,
                        r#type: local_type,
                    } = target.root
                {
                    self.lower_control_flow_stmt(*assignment.value, Some((local_id, local_type)));
                    return;
                }
                // Complex assignment targets fall back to the general resolved-place lowering path.
                let diverges = assignment.value.r#type == ResolvedType::Never;
                let value = Box::new(self.lower_expr(*assignment.value));
                let node = MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::Assignment(MirAssignment { target, value }),
                };
                if self.is_current_terminated() {
                    return;
                }
                self.push_stmt(node);
                // Honor divergence after the assignment fast path just as in general expression lowering.
                if diverges {
                    self.set_terminator(MirTerminator::Unreachable);
                }
                return;
            }
            let target = self.lower_place(assignment.target);
            let diverges = assignment.value.r#type == ResolvedType::Never;
            let value = Box::new(self.lower_expr(*assignment.value));
            let node = MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::Assignment(MirAssignment { target, value }),
            };
            if self.is_current_terminated() {
                return;
            }
            self.push_stmt(node);
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
            }
            return;
        }

        if is_control_flow_expr(&kind) {
            self.lower_control_flow_stmt(
                CheckedExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                },
                None,
            );
            return;
        }

        let diverges = r#type == ResolvedType::Never;
        let node = self.lower_expr(CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        });
        if self.is_current_terminated() {
            return;
        }
        self.push_stmt(node);
        // Terminate the block explicitly after a never-returning call.
        if diverges {
            self.set_terminator(MirTerminator::Unreachable);
        }
    }

    fn lower_function_body(&mut self, block: CheckedBlock) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        if let Some(tail) = block.tail {
            // Route return-position value control flow directly to the function result destination.
            if is_control_flow_expr(&tail.kind) {
                let result = self.return_slot;
                let result_type = self.return_type.clone();
                self.lower_control_flow_into(*tail, self.exit_chain_start, result, result_type);
                return false;
            }
            let diverges = tail.r#type == ResolvedType::Never;
            let value = self.lower_expr(*tail);
            if self.is_current_terminated() {
                return false;
            }
            if let Some(slot) = self.return_slot {
                let id = value.id;
                let span = value.span;
                let r#type = value.r#type.clone();
                self.assign_local(id, span, slot, r#type, value);
            }
            // Do not read a tail value from a branch that diverged.
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
                return false;
            }
        }
        true
    }

    fn lower_block_as_stmt(&mut self, block: CheckedBlock) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        if let Some(tail) = block.tail {
            let diverges = tail.r#type == ResolvedType::Never;
            let node = self.lower_expr(*tail);
            if self.is_current_terminated() {
                return false;
            }
            self.push_stmt(node);
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
                return false;
            }
        }
        true
    }

    fn lower_control_flow_into(
        &mut self,
        expr: CheckedExprNode,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        match expr.kind {
            CheckedExpr::If(CheckedIf {
                branches,
                else_branch,
            }) => self.lower_if_chain(
                branches.into_iter(),
                else_branch,
                merge,
                result,
                result_type,
            ),
            CheckedExpr::Match(CheckedMatch { arms, else_branch }) => {
                self.lower_match_chain(arms.into_iter(), else_branch, merge, result, result_type)
            }
            CheckedExpr::Codeblock(block) => {
                self.lower_block_into(block, merge, result, result_type)
            }
            _ => unreachable!(
                "lower_control_flow_into is only ever called after is_control_flow_expr matched"
            ),
        }
    }

    fn lower_control_flow_stmt(
        &mut self,
        expr: CheckedExprNode,
        result: Option<(LocalId, ResolvedType)>,
    ) {
        let merge = self.new_block();
        let (result_id, result_type) = match result {
            Some((id, r#type)) => (Some(id), r#type),
            None => (None, ResolvedType::Void),
        };
        let reached = self.lower_control_flow_into(expr, merge, result_id, result_type);
        self.current = merge;
        if !reached {
            self.set_terminator(MirTerminator::Unreachable);
        }
    }

    fn lower_block_into(
        &mut self,
        block: CheckedBlock,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        if let Some(tail) = block.tail {
            let diverges = tail.r#type == ResolvedType::Never;
            let value = self.lower_expr(*tail);
            if self.is_current_terminated() {
                return false;
            }
            match result {
                Some(result) => {
                    let id = value.id;
                    let span = value.span;
                    self.assign_local(id, span, result, result_type, value);
                }
                None => self.push_stmt(value),
            }
            // A diverging match arm contributes no value to the join.
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
                return false;
            }
        }
        self.set_terminator(MirTerminator::Goto(merge));
        true
    }

    fn lower_while(&mut self, loop_id: HirId, condition: CheckedExprNode, body: CheckedBlock) {
        let header = self.new_block();
        let body_blk = self.new_block();
        let exit = self.new_block();

        self.set_terminator(MirTerminator::Goto(header));

        self.current = header;
        let cond = self.lower_expr(condition);
        if self.is_current_terminated() {
            return;
        }
        self.set_terminator(MirTerminator::Branch {
            condition: cond,
            then_block: body_blk,
            else_block: exit,
        });

        self.current = body_blk;
        self.loop_stack.push((
            loop_id,
            LoopTargets {
                break_block: exit,
                continue_block: header,
            },
        ));
        let fell_through = self.lower_block_as_stmt(body);
        self.loop_stack.pop();
        if fell_through {
            self.set_terminator(MirTerminator::Goto(header));
        }

        self.current = exit;
    }

    fn lower_loop(&mut self, loop_id: HirId, body: CheckedBlock) {
        let body_blk = self.new_block();
        let exit = self.new_block();

        self.set_terminator(MirTerminator::Goto(body_blk));

        self.current = body_blk;
        self.loop_stack.push((
            loop_id,
            LoopTargets {
                break_block: exit,
                continue_block: body_blk,
            },
        ));
        let fell_through = self.lower_block_as_stmt(body);
        self.loop_stack.pop();
        if fell_through {
            self.set_terminator(MirTerminator::Goto(body_blk));
        }

        self.current = exit;
    }

    fn lower_for(
        &mut self,
        loop_id: HirId,
        init: Vec<CheckedStmt>,
        condition: CheckedExprNode,
        post: Option<CheckedExprNode>,
        body: CheckedBlock,
    ) {
        self.lower_stmts(init);
        if self.is_current_terminated() {
            return;
        }

        let header = self.new_block();
        let continue_blk = self.new_block();
        let body_blk = self.new_block();
        let exit = self.new_block();

        self.set_terminator(MirTerminator::Goto(header));

        self.current = header;
        let cond = self.lower_expr(condition);
        if self.is_current_terminated() {
            return;
        }
        self.set_terminator(MirTerminator::Branch {
            condition: cond,
            then_block: body_blk,
            else_block: exit,
        });

        self.current = body_blk;
        self.loop_stack.push((
            loop_id,
            LoopTargets {
                break_block: exit,
                continue_block: continue_blk,
            },
        ));
        let fell_through = self.lower_block_as_stmt(body);
        self.loop_stack.pop();
        if fell_through {
            self.set_terminator(MirTerminator::Goto(continue_blk));
        }

        self.current = continue_blk;
        if let Some(post) = post {
            let diverges = post.r#type == ResolvedType::Never;
            let node = self.lower_expr(post);
            if !self.is_current_terminated() {
                self.push_stmt(node);
                if diverges {
                    self.set_terminator(MirTerminator::Unreachable);
                }
            }
        }
        if !self.is_current_terminated() {
            self.set_terminator(MirTerminator::Goto(header));
        }

        self.current = exit;
    }

    pub(super) fn lower_expr(&mut self, node: CheckedExprNode) -> MirExprNode {
        let CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        } = node;
        match kind {
            CheckedExpr::If(CheckedIf {
                branches,
                else_branch,
            }) => self.lower_if_expr(id, span, r#type, branches, else_branch),
            CheckedExpr::Match(CheckedMatch { arms, else_branch }) => {
                self.lower_match_expr(id, span, r#type, arms, else_branch)
            }
            CheckedExpr::Codeblock(block) => self.lower_codeblock_expr(id, span, r#type, block),

            CheckedExpr::Place(place) => {
                let place = self.lower_place(place);
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::Place(place),
                }
            }
            CheckedExpr::Number(n) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::Number(n),
            },
            CheckedExpr::Bool(b) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::Bool(b),
            },
            CheckedExpr::Char(c) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::Char(c),
            },
            CheckedExpr::String(s) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::String(s),
            },
            CheckedExpr::ByteString(s) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::ByteString(s),
            },
            CheckedExpr::Const(v) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::Const(v),
            },
            CheckedExpr::Sizeof(t) => MirExprNode {
                id,
                span,
                r#type,
                kind: MirExpr::Sizeof(t),
            },

            CheckedExpr::FunctionCall(call) => {
                let callee = Box::new(self.lower_expr(*call.callee));
                let args = call.args.into_iter().map(|a| self.lower_expr(a)).collect();
                let kind = MirExpr::FunctionCall(MirFunctionCall {
                    callee,
                    fn_type: call.fn_type,
                    args,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::Assignment(a) => {
                let target = self.lower_place(a.target);
                let value = Box::new(self.lower_expr(*a.value));
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::Assignment(MirAssignment { target, value }),
                }
            }
            CheckedExpr::AddressOf(a) => {
                let place = self.lower_place(a.place);
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::AddressOf(MirAddressOf { place }),
                }
            }
            CheckedExpr::Negate(e) => {
                let e = Box::new(self.lower_expr(*e));
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::Negate(e),
                }
            }
            CheckedExpr::BitNot(e) => {
                let e = Box::new(self.lower_expr(*e));
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::BitNot(e),
                }
            }
            CheckedExpr::BinaryOp(b) => {
                let left = Box::new(self.lower_expr(*b.left));
                let right = Box::new(self.lower_expr(*b.right));
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::BinaryOp(MirBinaryOp {
                        op: b.op,
                        left,
                        right,
                    }),
                }
            }
            CheckedExpr::ArrayLiteral(lit) => {
                let elements = lit
                    .elements
                    .into_iter()
                    .map(|e| self.lower_expr(e))
                    .collect();
                let kind = MirExpr::ArrayLiteral(MirArrayLiteral {
                    item_type: lit.item_type,
                    elements,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::StructLiteral(lit) => {
                let fields = lit
                    .fields
                    .into_iter()
                    .map(|f| self.lower_field_init(f))
                    .collect();
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::StructLiteral(MirStructLiteral { fields }),
                }
            }
            CheckedExpr::EnumConstruct(construct) => {
                let fields = construct
                    .fields
                    .into_iter()
                    .map(|f| self.lower_field_init(f))
                    .collect();
                let kind = MirExpr::EnumConstruct(MirEnumConstruct {
                    variant_index: construct.variant_index,
                    fields,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::UnionConstruct(construct) => {
                let value = Box::new(self.lower_expr(*construct.value));
                let kind = MirExpr::UnionConstruct(MirUnionConstruct {
                    field_index: construct.field_index,
                    value,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::Slice(s) => {
                let base = self.lower_place(s.base);
                let start = s.start.map(|e| Box::new(self.lower_expr(*e)));
                let (end, inclusive) = match s.end {
                    CheckedRangeEnd::Inclusive(end) => {
                        (Some(Box::new(self.lower_expr(*end))), true)
                    }
                    CheckedRangeEnd::Exclusive(end) => {
                        (Some(Box::new(self.lower_expr(*end))), false)
                    }
                    CheckedRangeEnd::Open => (None, false),
                };
                let kind = MirExpr::Slice(MirSlice {
                    base,
                    item_type: s.item_type,
                    start,
                    end,
                    inclusive,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::Cast(cast) => {
                let base = Box::new(self.lower_expr(*cast.base));
                let kind = MirExpr::Cast(MirCast {
                    kind: cast.kind,
                    target_type: cast.target_type,
                    base,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::SpecCoerce(coerce) => {
                let base = Box::new(self.lower_expr(*coerce.base));
                let kind = MirExpr::SpecCoerce(MirSpecCoerce {
                    base,
                    slots: coerce.slots,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
            CheckedExpr::DynamicCall(call) => {
                let base = self.lower_place(call.base);
                let args = call.args.into_iter().map(|a| self.lower_expr(a)).collect();
                let kind = MirExpr::DynamicCall(MirDynamicCall {
                    base,
                    slot_index: call.slot_index,
                    fn_type: call.fn_type,
                    args,
                });
                MirExprNode {
                    id,
                    span,
                    r#type,
                    kind,
                }
            }
        }
    }

    fn lower_field_init(&mut self, field: CheckedStructLiteralField) -> MirFieldInit {
        MirFieldInit {
            field_index: field.field_index,
            value: self.lower_expr(field.value),
        }
    }

    fn finish_merge(
        &mut self,
        merge: BlockId,
        reached: bool,
        result: LocalId,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
    ) -> MirExprNode {
        self.current = merge;
        if !reached {
            self.set_terminator(MirTerminator::Unreachable);
        }
        let root = MirPlaceRoot::Local {
            id: result,
            r#type: r#type.clone(),
        };
        let kind = MirExpr::Place(MirPlace {
            root,
            projections: vec![],
            r#type: r#type.clone(),
            align: place_align(&r#type),
        });
        MirExprNode {
            id,
            span,
            r#type,
            kind,
        }
    }

    fn lower_codeblock_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        block: CheckedBlock,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let reached = self.lower_block_into(block, merge, Some(result), r#type.clone());
        self.finish_merge(merge, reached, result, id, span, r#type)
    }

    fn lower_if_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        branches: Vec<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let reached = self.lower_if_chain(
            branches.into_iter(),
            else_branch,
            merge,
            Some(result),
            r#type.clone(),
        );
        self.finish_merge(merge, reached, result, id, span, r#type)
    }

    fn lower_if_chain(
        &mut self,
        mut branches: std::vec::IntoIter<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        let Some((cond, then_body)) = branches.next() else {
            return match else_branch {
                Some(b) => self.lower_block_into(b, merge, result, result_type),
                None => {
                    self.set_terminator(MirTerminator::Goto(merge));
                    true
                }
            };
        };

        let cond = self.lower_expr(cond);
        if self.is_current_terminated() {
            return false;
        }
        let then_blk = self.new_block();
        let else_blk = self.new_block();
        self.set_terminator(MirTerminator::Branch {
            condition: cond,
            then_block: then_blk,
            else_block: else_blk,
        });

        self.current = then_blk;
        let then_reached = self.lower_block_into(then_body, merge, result, result_type.clone());
        self.current = else_blk;
        let else_reached = self.lower_if_chain(branches, else_branch, merge, result, result_type);
        then_reached || else_reached
    }

    fn lower_match_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        arms: Vec<CheckedMatchArm>,
        else_branch: Option<CheckedBlock>,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let reached = self.lower_match_chain(
            arms.into_iter(),
            else_branch,
            merge,
            Some(result),
            r#type.clone(),
        );
        self.finish_merge(merge, reached, result, id, span, r#type)
    }

    fn lower_match_chain(
        &mut self,
        mut arms: std::vec::IntoIter<CheckedMatchArm>,
        else_branch: Option<CheckedBlock>,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        let Some(arm) = arms.next() else {
            return match else_branch {
                Some(b) => self.lower_block_into(b, merge, result, result_type),
                None => {
                    self.set_terminator(MirTerminator::Unreachable);
                    false
                }
            };
        };

        // Each match arm is an OR of condition groups; each group itself short-circuits as AND.
        if arm.conditions.iter().any(|group| group.is_empty()) {
            return self.lower_block_into(arm.body, merge, result, result_type);
        }

        let body_blk = self.new_block();
        let fail_blk = self.new_block();
        let group_count = arm.conditions.len();
        let mut group_entry = self.current;
        for (g, group) in arm.conditions.into_iter().enumerate() {
            self.current = group_entry;
            // Successful conditions jump to the arm body; failed conditions advance to the next group.
            let group_fail = if g + 1 == group_count {
                fail_blk
            } else {
                self.new_block()
            };
            let condition_count = group.len();
            for (i, cond) in group.into_iter().enumerate() {
                let cond = self.lower_expr(cond);
                if self.is_current_terminated() {
                    return false;
                }
                let true_target = if i + 1 == condition_count {
                    body_blk
                } else {
                    self.new_block()
                };
                self.set_terminator(MirTerminator::Branch {
                    condition: cond,
                    then_block: true_target,
                    else_block: group_fail,
                });
                self.current = true_target;
            }
            group_entry = group_fail;
        }

        self.current = body_blk;
        let body_reached = self.lower_block_into(arm.body, merge, result, result_type.clone());
        self.current = fail_blk;
        let fail_reached = self.lower_match_chain(arms, else_branch, merge, result, result_type);
        body_reached || fail_reached
    }

    // Resolved place lowering is centralized in lower/place.rs.
    fn lower_place(&mut self, place: CheckedPlace) -> MirPlace {
        super::place::lower_place(self, place)
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

fn is_control_flow_expr(kind: &CheckedExpr) -> bool {
    matches!(
        kind,
        CheckedExpr::If(_) | CheckedExpr::Match(_) | CheckedExpr::Codeblock(_)
    )
}

fn collect_defer_ids(block: &CheckedBlock, out: &mut Vec<(HirId, Span)>) {
    for stmt in &block.stmts {
        collect_defer_ids_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        collect_defer_ids_expr(tail, out);
    }
}

fn collect_defer_ids_stmt(stmt: &CheckedStmt, out: &mut Vec<(HirId, Span)>) {
    match stmt {
        CheckedStmt::Declaration(_)
        | CheckedStmt::ExternDeclaration(_)
        | CheckedStmt::Break(_)
        | CheckedStmt::Continue(_) => {}
        CheckedStmt::Expression(e) | CheckedStmt::Return(e) => collect_defer_ids_expr(e, out),
        CheckedStmt::While(w) => {
            collect_defer_ids_expr(&w.condition, out);
            collect_defer_ids(&w.body, out);
        }
        CheckedStmt::Loop(l) => collect_defer_ids(&l.body, out),
        CheckedStmt::For(f) => {
            for s in &f.init {
                collect_defer_ids_stmt(s, out);
            }
            collect_defer_ids_expr(&f.condition, out);
            if let Some(post) = &f.post {
                collect_defer_ids_expr(post, out);
            }
            collect_defer_ids(&f.body, out);
        }
        CheckedStmt::Defer(d) => {
            out.push((d.id, d.span));
            // Nested defers are rejected earlier, so defer bodies should not introduce another exit chain.
            collect_defer_ids(&d.body, out);
        }
    }
}

fn collect_defer_ids_expr(expr: &CheckedExprNode, out: &mut Vec<(HirId, Span)>) {
    match &expr.kind {
        CheckedExpr::Number(_)
        | CheckedExpr::Bool(_)
        | CheckedExpr::Char(_)
        | CheckedExpr::String(_)
        | CheckedExpr::ByteString(_)
        | CheckedExpr::Const(_)
        | CheckedExpr::Sizeof(_) => {}
        CheckedExpr::Place(p) => collect_defer_ids_place(p, out),
        CheckedExpr::FunctionCall(call) => {
            collect_defer_ids_expr(&call.callee, out);
            for arg in &call.args {
                collect_defer_ids_expr(arg, out);
            }
        }
        CheckedExpr::Assignment(a) => {
            collect_defer_ids_place(&a.target, out);
            collect_defer_ids_expr(&a.value, out);
        }
        CheckedExpr::AddressOf(a) => collect_defer_ids_place(&a.place, out),
        CheckedExpr::Negate(e) => collect_defer_ids_expr(e, out),
        CheckedExpr::BitNot(e) => collect_defer_ids_expr(e, out),
        CheckedExpr::BinaryOp(b) => {
            collect_defer_ids_expr(&b.left, out);
            collect_defer_ids_expr(&b.right, out);
        }
        CheckedExpr::Codeblock(block) => collect_defer_ids(block, out),
        CheckedExpr::If(if_expr) => {
            for (cond, block) in &if_expr.branches {
                collect_defer_ids_expr(cond, out);
                collect_defer_ids(block, out);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                collect_defer_ids(else_branch, out);
            }
        }
        CheckedExpr::ArrayLiteral(lit) => {
            for e in &lit.elements {
                collect_defer_ids_expr(e, out);
            }
        }
        CheckedExpr::StructLiteral(lit) => {
            for f in &lit.fields {
                collect_defer_ids_expr(&f.value, out);
            }
        }
        CheckedExpr::EnumConstruct(construct) => {
            for f in &construct.fields {
                collect_defer_ids_expr(&f.value, out);
            }
        }
        CheckedExpr::Slice(s) => {
            collect_defer_ids_place(&s.base, out);
            if let Some(start) = &s.start {
                collect_defer_ids_expr(start, out);
            }
            match &s.end {
                CheckedRangeEnd::Inclusive(end) => collect_defer_ids_expr(end, out),
                CheckedRangeEnd::Exclusive(end) => collect_defer_ids_expr(end, out),
                _ => {}
            }
        }
        CheckedExpr::Match(m) => {
            for arm in &m.arms {
                for group in &arm.conditions {
                    for cond in group {
                        collect_defer_ids_expr(cond, out);
                    }
                }
                collect_defer_ids(&arm.body, out);
            }
            if let Some(else_branch) = &m.else_branch {
                collect_defer_ids(else_branch, out);
            }
        }
        CheckedExpr::Cast(cast) => collect_defer_ids_expr(&cast.base, out),
        CheckedExpr::UnionConstruct(construct) => collect_defer_ids_expr(&construct.value, out),
        CheckedExpr::SpecCoerce(coerce) => collect_defer_ids_expr(&coerce.base, out),
        CheckedExpr::DynamicCall(call) => {
            collect_defer_ids_place(&call.base, out);
            for arg in &call.args {
                collect_defer_ids_expr(arg, out);
            }
        }
    }
}

fn collect_defer_ids_place(place: &CheckedPlace, out: &mut Vec<(HirId, Span)>) {
    if let CheckedPlaceRoot::Expr(e) = &place.root {
        collect_defer_ids_expr(e, out);
    }
    for proj in &place.projections {
        if let CheckedProjection::Index { index_expr, .. } = proj {
            collect_defer_ids_expr(index_expr, out);
        }
    }
}
