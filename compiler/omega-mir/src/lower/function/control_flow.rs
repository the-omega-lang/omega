use super::{BlockDestination, FunctionLowerer, LoopTargets, is_control_flow_expr};
use crate::body::{
    MirBinaryOp, MirEnumConstruct, MirExpr, MirExprNode, MirFieldInit, MirPlace, MirProjection,
    MirSpecCoerce, MirTerminator,
};
use crate::ids::{BlockId, LocalId};
use crate::lower::place::place_align;
use omega_analyzer::checked::{
    CheckedAnonymousEnumWiden, CheckedBlock, CheckedCoercion, CheckedCoercionStep, CheckedExpr,
    CheckedExprNode, CheckedMatchArm, CheckedStmt, CheckedTry, CheckedTryDestination,
    CheckedTrySource, NumberValue,
};
use omega_analyzer::layout::ANONYMOUS_ENUM_TAG_TYPE;
use omega_analyzer::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::{BinaryOp, Span};

impl FunctionLowerer {
    pub(super) fn lower_control_flow_into(
        &mut self,
        expr: CheckedExprNode,
        destination: BlockDestination,
    ) -> bool {
        let CheckedExprNode {
            id,
            span,
            r#type,
            kind,
        } = expr;
        match kind {
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
            CheckedExpr::Try(r#try) => self.lower_try_into(id, span, r#type, r#try, destination),
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

    /// Rebuilds an anonymous-enum value under a wider shape by branching on
    /// the source tag and constructing the mapped destination member. There
    /// is no bit-level shortcut: the two shapes order their members
    /// independently, and the payload can move.
    pub(super) fn lower_anonymous_enum_widen(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        widen: CheckedAnonymousEnumWiden,
    ) -> MirExprNode {
        let source = self.lower_expr(*widen.source);
        self.widen_anonymous_enum(id, span, r#type, source, widen.variant_map)
    }

    fn widen_anonymous_enum(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        source: MirExprNode,
        variant_map: Vec<usize>,
    ) -> MirExprNode {
        let ResolvedType::AnonymousEnum {
            shape: source_shape,
            ..
        } = source.r#type.clone()
        else {
            unreachable!("checked module guarantees an anonymous-enum widening source")
        };
        // The source must be read once for the tag and again for the payload,
        // so any side effect in it happens before either read.
        let source = self.materialize_once(source);
        let MirExpr::Place(source_place) = source.kind else {
            unreachable!("omega-mir lowering bug: materialize_once always yields a place read")
        };
        let tag = MirExprNode {
            id,
            span,
            r#type: ANONYMOUS_ENUM_TAG_TYPE,
            kind: MirExpr::Place(projected(
                &source_place,
                MirProjection::EnumTag {
                    r#type: ANONYMOUS_ENUM_TAG_TYPE,
                },
                ANONYMOUS_ENUM_TAG_TYPE,
            )),
        };
        let tag = self.materialize_once(tag);

        let result = self.declare_local(None, r#type);
        let merge = self.new_block();
        for (source_index, target_index) in variant_map.into_iter().enumerate() {
            let member = source_shape.members()[source_index].clone();
            let body = MirExprNode {
                id,
                span,
                r#type: member.clone(),
                kind: MirExpr::Place(projected(
                    &source_place,
                    MirProjection::EnumBody {
                        variant_index: source_index,
                        field_index: 0,
                        r#type: member.clone(),
                    },
                    member,
                )),
            };
            let constructed = MirExprNode {
                id,
                span,
                r#type: self.local_type(result).clone(),
                kind: MirExpr::EnumConstruct(MirEnumConstruct {
                    variant_index: target_index,
                    fields: vec![MirFieldInit {
                        field_index: 0,
                        value: body,
                    }],
                }),
            };
            let matched = self.new_block();
            let next = self.new_block();
            self.terminate(MirTerminator::Branch {
                condition: MirExprNode {
                    id,
                    span,
                    r#type: ResolvedType::Bool,
                    kind: MirExpr::BinaryOp(MirBinaryOp {
                        op: BinaryOp::Eq,
                        left: Box::new(tag.clone()),
                        right: Box::new(MirExprNode {
                            id,
                            span,
                            r#type: ANONYMOUS_ENUM_TAG_TYPE,
                            kind: MirExpr::Number(NumberValue::Unsigned(source_index as u64)),
                        }),
                    }),
                },
                then_block: matched,
                else_block: next,
            });

            self.current = matched;
            self.assign_local(id, span, result, constructed);
            self.terminate(MirTerminator::Goto(merge));
            self.current = next;
        }
        // The source tag is always one of its own canonical indices.
        self.terminate(MirTerminator::Unreachable);
        self.current = merge;
        self.local_expr(result, id, span)
    }

    pub(super) fn lower_try_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        r#try: CheckedTry,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let destination = BlockDestination::new(merge, Some(result));
        let reached = self.lower_try_into(id, span, r#type, r#try, destination);
        self.finish_merge(merge, reached, result, id, span)
    }

    /// Turns `operand?` into ordinary control flow. The operand is stored
    /// once and then read for both its tag and its payload; the failure
    /// variant builds the enclosing function's failure value and enters the
    /// existing return/defer exit chain; the success variant's payload
    /// continues into `destination`. Every semantic fact used here was
    /// already resolved by the analyzer.
    fn lower_try_into(
        &mut self,
        id: HirId,
        span: Span,
        success_type: ResolvedType,
        r#try: CheckedTry,
        destination: BlockDestination,
    ) -> bool {
        let operand = self.lower_expr(*r#try.operand);
        if self.is_current_terminated() {
            return false;
        }
        let operand = self.materialize_once(operand);
        let MirExpr::Place(operand_place) = operand.kind else {
            unreachable!("omega-mir lowering bug: materialize_once always yields a place read")
        };

        let source = r#try.source;
        let tag_type = source.tag_type.clone();
        let tag = MirExprNode {
            id,
            span,
            r#type: tag_type.clone(),
            kind: MirExpr::Place(projected(
                &operand_place,
                MirProjection::EnumTag {
                    r#type: tag_type.clone(),
                },
                tag_type.clone(),
            )),
        };
        let condition = MirExprNode {
            id,
            span,
            r#type: ResolvedType::Bool,
            kind: MirExpr::BinaryOp(MirBinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(tag),
                right: Box::new(MirExprNode {
                    id,
                    span,
                    r#type: tag_type,
                    kind: MirExpr::Number(source.success_tag),
                }),
            }),
        };
        let success_block = self.new_block();
        let failure_block = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: success_block,
            else_block: failure_block,
        });

        self.current = failure_block;
        self.lower_try_failure(id, span, &operand_place, &source, r#try.destination);

        self.current = success_block;
        let payload = MirExprNode {
            id,
            span,
            r#type: success_type.clone(),
            kind: MirExpr::Place(projected(
                &operand_place,
                MirProjection::EnumBody {
                    variant_index: source.success_variant,
                    field_index: source.success_field,
                    r#type: success_type.clone(),
                },
                success_type,
            )),
        };
        match destination.result {
            Some(result) => self.assign_local(id, span, result, payload),
            None => self.push_stmt(payload),
        }
        self.terminate(MirTerminator::Goto(destination.merge));
        true
    }

    fn lower_try_failure(
        &mut self,
        id: HirId,
        span: Span,
        operand_place: &MirPlace,
        source: &CheckedTrySource,
        destination: CheckedTryDestination,
    ) {
        let fields = match (&source.failure_payload, destination.failure_field) {
            (Some((source_field, error_type)), Some(field_index)) => {
                let error = MirExprNode {
                    id,
                    span,
                    r#type: error_type.clone(),
                    kind: MirExpr::Place(projected(
                        operand_place,
                        MirProjection::EnumBody {
                            variant_index: source.failure_variant,
                            field_index: *source_field,
                            r#type: error_type.clone(),
                        },
                        error_type.clone(),
                    )),
                };
                let value = self.apply_coercion(id, span, &destination.error_coercion, error);
                vec![MirFieldInit { field_index, value }]
            }
            _ => Vec::new(),
        };
        let failure = MirExprNode {
            id,
            span,
            r#type: destination.r#type,
            kind: MirExpr::EnumConstruct(MirEnumConstruct {
                variant_index: destination.failure_variant,
                fields,
            }),
        };
        self.return_value(failure);
    }

    /// Replays a coercion the analyzer already decided on. Nothing here
    /// re-asks whether the conversion is legal.
    fn apply_coercion(
        &mut self,
        id: HirId,
        span: Span,
        plan: &CheckedCoercion,
        value: MirExprNode,
    ) -> MirExprNode {
        let mut value = value;
        for step in &plan.steps {
            value = match step {
                CheckedCoercionStep::ProjectAnonymousMember {
                    variant_index,
                    member_type,
                } => {
                    let base = match value.kind {
                        MirExpr::Place(place) => place,
                        kind => {
                            let node = MirExprNode { kind, ..value };
                            let stored = self.materialize_once(node);
                            let MirExpr::Place(place) = stored.kind else {
                                unreachable!(
                                    "omega-mir lowering bug: materialize_once always yields a place read"
                                )
                            };
                            place
                        }
                    };
                    MirExprNode {
                        id,
                        span,
                        r#type: member_type.clone(),
                        kind: MirExpr::Place(projected(
                            &base,
                            MirProjection::EnumBody {
                                variant_index: *variant_index,
                                field_index: 0,
                                r#type: member_type.clone(),
                            },
                            member_type.clone(),
                        )),
                    }
                }
                CheckedCoercionStep::InjectAnonymousMember {
                    variant_index,
                    target_type,
                } => MirExprNode {
                    id,
                    span,
                    r#type: target_type.clone(),
                    kind: MirExpr::EnumConstruct(MirEnumConstruct {
                        variant_index: *variant_index,
                        fields: vec![MirFieldInit {
                            field_index: 0,
                            value,
                        }],
                    }),
                },
                CheckedCoercionStep::WidenAnonymousEnum {
                    variant_map,
                    target_type,
                } => self.widen_anonymous_enum(
                    id,
                    span,
                    target_type.clone(),
                    value,
                    variant_map.clone(),
                ),
                CheckedCoercionStep::SpecCoerce { slots, target_type } => MirExprNode {
                    id,
                    span,
                    r#type: target_type.clone(),
                    kind: MirExpr::SpecCoerce(MirSpecCoerce {
                        base: Box::new(value),
                        slots: slots.clone(),
                    }),
                },
            };
        }
        value
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

fn projected(base: &MirPlace, projection: MirProjection, r#type: ResolvedType) -> MirPlace {
    let mut projections = base.projections.clone();
    projections.push(projection);
    MirPlace {
        root: base.root.clone(),
        projections,
        align: place_align(&r#type),
        r#type,
    }
}
