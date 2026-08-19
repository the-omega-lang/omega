use super::{BlockDestination, FunctionLowerer, LoopTargets, is_control_flow_expr};
use crate::body::{MirExprNode, MirTerminator};
use crate::ids::{BlockId, LocalId};
use omega_analyzer::checked::{
    CheckedBlock, CheckedExpr, CheckedExprNode, CheckedMatchArm, CheckedStmt,
};
use omega_analyzer::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::Span;

impl FunctionLowerer {
    pub(super) fn lower_control_flow_into(
        &mut self,
        expr: CheckedExprNode,
        destination: BlockDestination,
    ) -> bool {
        match expr.kind {
            CheckedExpr::If(if_expr) => self.lower_if_chain(
                if_expr.branches.into_iter(),
                if_expr.else_branch,
                destination,
            ),
            CheckedExpr::Match(match_expr) => self.lower_match_chain(
                match_expr.arms.into_iter(),
                match_expr.else_branch,
                destination,
            ),
            CheckedExpr::Codeblock(block) => self.lower_block_into(block, destination),
            _ => unreachable!(
                "lower_control_flow_into is only called for checked control-flow expressions"
            ),
        }
    }

    pub(super) fn lower_control_flow_stmt(
        &mut self,
        expr: CheckedExprNode,
        result: Option<LocalId>,
    ) {
        let merge = self.new_block();
        let reached = self.lower_control_flow_into(expr, BlockDestination::new(merge, result));
        self.current = merge;
        if !reached {
            self.terminate(MirTerminator::Unreachable);
        }
    }

    fn lower_block_into(&mut self, block: CheckedBlock, destination: BlockDestination) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }

        if let Some(tail) = block.tail {
            if is_control_flow_expr(&tail.kind) {
                return self.lower_control_flow_into(*tail, destination);
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

            match destination.result {
                Some(result) => self.assign_local(value.id, value.span, result, value),
                None => self.push_stmt(value),
            }
        }

        self.terminate(MirTerminator::Goto(destination.merge));
        true
    }

    pub(super) fn lower_while(
        &mut self,
        loop_id: HirId,
        condition: CheckedExprNode,
        body: CheckedBlock,
    ) {
        let header = self.new_block();
        self.terminate(MirTerminator::Goto(header));

        self.current = header;
        let condition = self.lower_expr(condition);
        if self.is_current_terminated() {
            return;
        }

        // Allocate successors only after the checked condition has been lowered. This keeps
        // malformed/diverging checked input from leaving reserved blocks unterminated.
        let body_block = self.new_block();
        let exit = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: body_block,
            else_block: exit,
        });

        self.current = body_block;
        self.push_loop(
            loop_id,
            LoopTargets {
                break_block: exit,
                continue_block: header,
            },
        );
        let fell_through = self.lower_block_as_stmt(body);
        self.pop_loop(loop_id);
        if fell_through {
            self.terminate(MirTerminator::Goto(header));
        }

        self.current = exit;
    }

    pub(super) fn lower_loop(&mut self, loop_id: HirId, body: CheckedBlock, has_break: bool) {
        let body_block = self.new_block();
        let exit = self.new_block();
        self.terminate(MirTerminator::Goto(body_block));

        self.current = body_block;
        self.push_loop(
            loop_id,
            LoopTargets {
                break_block: exit,
                continue_block: body_block,
            },
        );
        let fell_through = self.lower_block_as_stmt(body);
        self.pop_loop(loop_id);
        if fell_through {
            self.terminate(MirTerminator::Goto(body_block));
        }

        self.current = exit;
        if !has_break {
            self.terminate(MirTerminator::Unreachable);
        }
    }

    pub(super) fn lower_for(
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
        self.terminate(MirTerminator::Goto(header));

        self.current = header;
        let condition = self.lower_expr(condition);
        if self.is_current_terminated() {
            return;
        }

        let continue_block = self.new_block();
        let body_block = self.new_block();
        let exit = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: body_block,
            else_block: exit,
        });

        self.current = body_block;
        self.push_loop(
            loop_id,
            LoopTargets {
                break_block: exit,
                continue_block,
            },
        );
        let fell_through = self.lower_block_as_stmt(body);
        self.pop_loop(loop_id);
        if fell_through {
            self.terminate(MirTerminator::Goto(continue_block));
        }

        self.current = continue_block;
        if let Some(post) = post {
            if is_control_flow_expr(&post.kind) {
                self.lower_control_flow_stmt(post, None);
            } else {
                self.lower_plain_expr_stmt(post);
            }
        }
        if !self.is_current_terminated() {
            self.terminate(MirTerminator::Goto(header));
        }

        self.current = exit;
    }

    pub(super) fn lower_codeblock_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        block: CheckedBlock,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type);
        let reached = self.lower_block_into(block, BlockDestination::new(merge, Some(result)));
        self.finish_merge(merge, reached, result, id, span)
    }

    pub(super) fn lower_if_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        branches: Vec<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type);
        let destination = BlockDestination::new(merge, Some(result));
        let reached = self.lower_if_chain(branches.into_iter(), else_branch, destination);
        self.finish_merge(merge, reached, result, id, span)
    }

    fn lower_if_chain(
        &mut self,
        mut branches: std::vec::IntoIter<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
        destination: BlockDestination,
    ) -> bool {
        let Some((condition, then_body)) = branches.next() else {
            return match else_branch {
                Some(block) => self.lower_block_into(block, destination),
                None => {
                    self.terminate(MirTerminator::Goto(destination.merge));
                    true
                }
            };
        };

        let condition = self.lower_expr(condition);
        if self.is_current_terminated() {
            return false;
        }

        let then_block = self.new_block();
        let else_block = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block,
            else_block,
        });

        self.current = then_block;
        let then_reached = self.lower_block_into(then_body, destination);

        self.current = else_block;
        let else_reached = self.lower_if_chain(branches, else_branch, destination);
        then_reached || else_reached
    }

    pub(super) fn lower_match_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        arms: Vec<CheckedMatchArm>,
        else_branch: Option<CheckedBlock>,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type);
        let destination = BlockDestination::new(merge, Some(result));
        let reached = self.lower_match_chain(arms.into_iter(), else_branch, destination);
        self.finish_merge(merge, reached, result, id, span)
    }

    fn lower_match_chain(
        &mut self,
        mut arms: std::vec::IntoIter<CheckedMatchArm>,
        else_branch: Option<CheckedBlock>,
        destination: BlockDestination,
    ) -> bool {
        let Some(arm) = arms.next() else {
            return match else_branch {
                Some(block) => self.lower_block_into(block, destination),
                None => {
                    self.terminate(MirTerminator::Unreachable);
                    false
                }
            };
        };

        // Each arm is an OR of condition groups; each group short-circuits as AND.
        if arm.conditions.iter().any(|group| group.is_empty()) {
            return self.lower_block_into(arm.body, destination);
        }

        let body_block = self.new_block();
        let fail_block = self.new_block();
        let group_count = arm.conditions.len();
        let mut group_entry = self.current;

        for (group_index, group) in arm.conditions.into_iter().enumerate() {
            self.current = group_entry;
            let group_fail = if group_index + 1 == group_count {
                fail_block
            } else {
                self.new_block()
            };
            let condition_count = group.len();

            for (condition_index, condition) in group.into_iter().enumerate() {
                let condition = self.lower_expr(condition);
                assert!(
                    !self.is_current_terminated(),
                    "omega-mir lowering bug: a checked match condition unexpectedly diverged"
                );
                let true_target = if condition_index + 1 == condition_count {
                    body_block
                } else {
                    self.new_block()
                };
                self.terminate(MirTerminator::Branch {
                    condition,
                    then_block: true_target,
                    else_block: group_fail,
                });
                self.current = true_target;
            }
            group_entry = group_fail;
        }

        self.current = body_block;
        let body_reached = self.lower_block_into(arm.body, destination);

        self.current = fail_block;
        let fail_reached = self.lower_match_chain(arms, else_branch, destination);
        body_reached || fail_reached
    }

    fn finish_merge(
        &mut self,
        merge: BlockId,
        reached: bool,
        result: LocalId,
        id: HirId,
        span: Span,
    ) -> MirExprNode {
        self.current = merge;
        if !reached {
            self.terminate(MirTerminator::Unreachable);
        }
        self.local_expr(result, id, span)
    }
}
