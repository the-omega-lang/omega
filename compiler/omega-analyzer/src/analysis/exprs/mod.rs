use super::*;

mod operators;
mod ranges;

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_expr(
        &mut self,
        node: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let id = node.id;
        let span = node.span;
        let literal = |r#type, kind| {
            Some(CheckedExprNode {
                id,
                span,
                r#type,
                kind,
            })
        };

        match &node.expr {
            HirExpr::Place(place) => self.analyze_place_read(id, span, place, expected),
            HirExpr::Reveal(inner) => self.analyze_reveal(id, span, inner, expected),
            HirExpr::Comp(inner) => self.analyze_comp(id, span, inner, expected),
            HirExpr::Number(number) => self.analyze_number(id, span, number, expected),
            HirExpr::Bool(b) => literal(ResolvedType::Bool, CheckedExpr::Bool(*b)),
            HirExpr::Char(c) => literal(ResolvedType::Char, CheckedExpr::Char(*c)),

            HirExpr::String(s) => literal(
                ResolvedType::Str { mutable: false },
                CheckedExpr::String(s.0.clone()),
            ),

            HirExpr::ByteString(s) => literal(
                ResolvedType::Slice {
                    item: Box::new(ResolvedType::U8),
                    mutable: false,
                },
                CheckedExpr::ByteString(s.0.clone()),
            ),

            HirExpr::Codeblock(block) => {
                let checked = self.analyze_block(block, expected)?;
                let r#type = Self::block_type(&checked).unwrap_or(ResolvedType::Void);
                literal(r#type, CheckedExpr::Codeblock(checked))
            }

            HirExpr::Sizeof(target) => {
                let target_type = self.resolve_type_or_error(id, span, target, true)?;
                literal(ResolvedType::USize, CheckedExpr::Sizeof(target_type))
            }

            HirExpr::If(HirIf {
                branches,
                else_branch,
            }) => self.analyze_if(id, span, branches, else_branch.as_ref(), expected),
            HirExpr::FunctionCall(call) => self.analyze_call(id, span, call, expected),
            HirExpr::Assignment(assignment) => self.analyze_assignment(id, span, assignment),
            HirExpr::CompoundAssign(HirCompoundAssign { target, op, value }) => {
                self.analyze_compound_assign(id, span, target, *op, value)
            }
            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                self.analyze_address_of(id, span, base, *mutable, expected)
            }
            HirExpr::Negate(base) => self.analyze_negate(id, span, base, expected),
            HirExpr::BitNot(base) => self.analyze_bit_not(id, span, base, expected),
            HirExpr::Not(base) => self.analyze_not(id, span, base),
            HirExpr::Logical(logical) => self.analyze_logical(id, span, logical),
            HirExpr::Increment(base) => self.analyze_incr_decr(id, span, base, BinaryOp::Add),
            HirExpr::Decrement(base) => self.analyze_incr_decr(id, span, base, BinaryOp::Sub),
            HirExpr::BinaryOp(bin) => self.analyze_binary_expr(id, span, bin, expected),
            HirExpr::ArrayLiteral(elements) => {
                self.analyze_array_literal(id, span, elements, expected)
            }
            HirExpr::StructLiteral(lit) => self.analyze_struct_literal(id, span, lit, expected),
            HirExpr::Match(m) => self.analyze_match(id, span, m, expected),
            HirExpr::Cast(HirCast { target, base }) => self.analyze_cast(id, span, target, base),

            HirExpr::Slice(_) => {
                self.error(id, span, AnalysisErrorKind::SliceRequiresAddressOf);
                None
            }

            HirExpr::Range(range) => self.analyze_range_value(id, span, range, expected),
        }
    }

    fn analyze_place_read(
        &mut self,
        id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (mut checked_place, mut r#type, _mutable) =
            self.analyze_place(id, span, place, expected)?;
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked_place.root {
            self.context.mark_used(decl_id);
        }
        // A refined anonymous binding read as a value *is* the member it
        // proves -- that is the only thing an anonymous enum's refinement can
        // offer, since the type has no members of its own. The exception is a
        // site that asked for the anonymous enum itself: widening back to the
        // parent must stay a plain read of the same storage, so the
        // refinement is kept and `accepts` handles it.
        //
        // Deliberately not in `analyze_place`: an assignment target and
        // `&mut` need the anonymous root, or a mutable alias into the payload
        // could outlive the proof that the payload holds that member.
        if let Some((index, member)) = r#type.refined_anonymous_member()
            && !expected.is_some_and(|expected| expected.accepts(&r#type))
        {
            let member = member.clone();
            checked_place.projections.push(CheckedProjection::EnumBody {
                variant_index: index,
                field_index: 0,
                r#type: member.clone(),
            });
            checked_place.r#type = member.clone();
            r#type = member;
        }
        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = checked_place.root
        {
            let value = self.resolve_comp_place(id, span, &checked_place)?;
            return Some(CheckedExprNode {
                id,
                span,
                r#type,
                kind: CheckedExpr::Const(value),
            });
        }
        Some(CheckedExprNode {
            id,
            span,
            r#type,
            kind: CheckedExpr::Place(checked_place),
        })
    }

    pub(super) fn resolve_comp_place(
        &mut self,
        id: HirId,
        span: Span,
        checked_place: &CheckedPlace,
    ) -> Option<ConstValue> {
        let CheckedPlaceRoot::Variable {
            decl_id,
            storage: Storage::Comp,
            ..
        } = checked_place.root
        else {
            unreachable!("resolve_comp_place is only ever called on a Storage::Comp place root");
        };
        crate::dead_code::collect_place(checked_place, &mut self.field_usage);
        let mut value = self
            .context
            .comp_value(decl_id)
            .cloned()
            .or_else(|| self.resolver.resolve_comp_value(decl_id))
            .expect("Storage::Comp is only ever produced alongside a recorded comp value, local or global");
        for proj in &checked_place.projections {
            value = self.apply_comp_projection(id, span, value, proj)?;
        }
        Some(value)
    }

    pub(super) fn apply_comp_projection(
        &mut self,
        id: HirId,
        span: Span,
        value: ConstValue,
        proj: &CheckedProjection,
    ) -> Option<ConstValue> {
        let unsupported = |this: &mut Self, reason: &str| -> Option<ConstValue> {
            this.error(
                id,
                span,
                AnalysisErrorKind::CompEvalFailed {
                    reason: reason.into(),
                    trace: vec![],
                },
            );
            None
        };
        match proj {
            CheckedProjection::FieldAccess { index, .. } => match value {
                ConstValue::Struct(fields) => Some(fields[*index].clone()),
                _ => unsupported(self, "field access on a non-struct comp value"),
            },
            CheckedProjection::Index { index_expr, .. } => {
                let index_value = self.eval_comp(id, index_expr)?;
                let index = match index_value {
                    ConstValue::Number(NumberValue::Unsigned(n)) => n as usize,
                    ConstValue::Number(NumberValue::Signed(n)) if n >= 0 => n as usize,
                    _ => return unsupported(self, "a non-integer comp index"),
                };
                match value {
                    ConstValue::Array(v) | ConstValue::Slice(v) => match v.get(index) {
                        Some(v) => Some(v.clone()),
                        None => unsupported(self, "an out-of-range comp index"),
                    },
                    _ => unsupported(self, "indexing a non-array/slice comp value"),
                }
            }
            CheckedProjection::SliceLength => match value {
                ConstValue::Slice(v) | ConstValue::Array(v) => {
                    Some(ConstValue::Number(NumberValue::Unsigned(v.len() as u64)))
                }
                ConstValue::Str(s) => {
                    Some(ConstValue::Number(NumberValue::Unsigned(s.len() as u64)))
                }
                _ => unsupported(self, "length of a non-slice/str comp value"),
            },
            CheckedProjection::EnumTag { .. } => match value {
                ConstValue::Enum { tag, .. } => Some(ConstValue::Number(tag)),
                _ => unsupported(self, "tag access on a non-enum comp value"),
            },
            CheckedProjection::EnumHeader { index, .. } => match value {
                ConstValue::Enum { header, .. } => Some(header[*index].clone()),
                _ => unsupported(self, "header access on a non-enum comp value"),
            },
            CheckedProjection::EnumDynamicField { index, .. } => match value {
                ConstValue::Enum { dynamic_fields, .. } => Some(dynamic_fields[*index].clone()),
                _ => unsupported(self, "dynamic-field access on a non-enum comp value"),
            },
            CheckedProjection::EnumBody { field_index, .. } => match value {
                ConstValue::Enum { fields, .. } => Some(fields[*field_index].clone()),
                _ => unsupported(self, "body-field access on a non-enum comp value"),
            },
            CheckedProjection::UnionField { index, .. } => match value {
                ConstValue::Union { field_index, value } if field_index == *index => Some(*value),
                ConstValue::Union { .. } => {
                    unsupported(self, "reading a union through its inactive field")
                }
                _ => unsupported(self, "field access on a non-union comp value"),
            },
            // No real memory for a `comp` value to dereference through.
            CheckedProjection::Deref { .. } => unsupported(
                self,
                "dereferencing a pointer inside a 'comp' binding projection isn't supported yet",
            ),
            CheckedProjection::SpecObjectPtr { .. } | CheckedProjection::SpecObjectVtable => {
                unsupported(
                    self,
                    "accessing a spec object's pointer/vtable inside a 'comp' evaluation isn't supported",
                )
            }
        }
    }

    fn analyze_reveal(
        &mut self,
        id: HirId,
        span: Span,
        inner: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        self.reveals.begin();
        let result = self.analyze_expr(inner, expected);
        if !self.reveals.finish() {
            self.warn(id, span, AnalysisWarningKind::UnnecessaryReveal);
        }
        result
    }

    fn analyze_comp(
        &mut self,
        id: HirId,
        span: Span,
        inner: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let checked = self.analyze_expr(inner, expected)?;
        let r#type = checked.r#type.clone();
        let value = self.eval_comp(id, &checked)?;
        Some(CheckedExprNode {
            id,
            span,
            r#type,
            kind: CheckedExpr::Const(value),
        })
    }

    pub(super) fn eval_comp(
        &mut self,
        id: HirId,
        expr: &CheckedExprNode,
    ) -> Option<crate::resolved_type::ConstValue> {
        crate::dead_code::collect_expr(expr, &mut self.field_usage);
        match crate::comp_eval::eval(self.resolver, expr, self.target) {
            Ok(value) => Some(value),
            Err(err) => {
                self.error(
                    id,
                    err.span,
                    AnalysisErrorKind::CompEvalFailed {
                        reason: err.kind.to_string(),
                        trace: err.trace,
                    },
                );
                None
            }
        }
    }

    fn analyze_if(
        &mut self,
        node_id: HirId,
        span: Span,
        branches: &[(HirExprNode, HirBlock)],
        else_branch: Option<&HirBlock>,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let has_else = else_branch.is_some();

        let mut checked_conds = Vec::with_capacity(branches.len());
        let mut checked_blocks: Vec<CheckedBlock> = Vec::with_capacity(branches.len());
        let mut anchor: Option<ResolvedType> = None;
        for (i, (cond, block)) in branches.iter().enumerate() {
            let checked_cond = self.analyze_expr(cond, None)?;
            if checked_cond.r#type != ResolvedType::Bool {
                self.error(
                    node_id,
                    checked_cond.span,
                    AnalysisErrorKind::NonBoolCondition {
                        r#type: checked_cond.r#type,
                    },
                );
                return None;
            }
            checked_conds.push(checked_cond);
            let block_expected = if !has_else {
                None
            } else if i == 0 {
                expected
            } else {
                anchor.as_ref()
            };
            let checked_block = self.analyze_block(block, block_expected)?;
            if has_else && i == 0 {
                anchor = Some(match expected {
                    Some(t) => t.clone(),
                    None => Self::block_type(&checked_block)
                        .map(|t| t.widened())
                        .unwrap_or(ResolvedType::Void),
                });
            }
            checked_blocks.push(checked_block);
        }
        let checked_else = match else_branch {
            Some(b) => Some(self.analyze_block(b, anchor.as_ref())?),
            None => None,
        };

        let checked_branches: Vec<(CheckedExprNode, CheckedBlock)> =
            checked_conds.into_iter().zip(checked_blocks).collect();

        let branch_kinds: Vec<Option<ResolvedType>> = checked_branches
            .iter()
            .map(|(_, b)| Self::block_type(b))
            .collect();
        let else_kind: Option<Option<ResolvedType>> = checked_else.as_ref().map(Self::block_type);

        let result_type = match &else_kind {
            Some(k) => branch_kinds
                .iter()
                .cloned()
                .chain(std::iter::once(k.clone()))
                .flatten()
                .next(),
            None => None,
        }
        .map(|t| t.widened())
        .unwrap_or(ResolvedType::Void);

        let mismatch = branch_kinds
            .iter()
            .cloned()
            .chain(else_kind.iter().cloned())
            .flatten()
            .find(|t| !result_type.accepts(t));
        if let Some(found) = mismatch {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::IfBranchTypeMismatch {
                    expected: result_type,
                    found,
                },
            );
            return None;
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: result_type,
            kind: CheckedExpr::If(CheckedIf {
                branches: checked_branches,
                else_branch: checked_else,
            }),
        })
    }

    fn analyze_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (reveal_depth, _) = Self::strip_reveal(&call.callee);
        if reveal_depth != 0 {
            return self.with_reveal_bypass(
                reveal_depth,
                call.callee.id,
                call.callee.span,
                |this| this.analyze_call_core(node_id, span, call, expected),
            );
        }
        self.analyze_call_core(node_id, span, call, expected)
    }

    fn analyze_call_core(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let interceptors: [Interceptor<'r>; 5] = [
            Self::resolve_spec_qualified_call,
            Self::resolve_overloaded_call,
            Self::resolve_overloaded_static_call,
            Self::resolve_generic_call,
            Self::resolve_generic_static_call,
        ];
        for intercept in interceptors {
            if let Intercepted::Claimed(result) = intercept(self, node_id, span, call, expected) {
                return result;
            }
        }

        let ResolvedCallee {
            callee,
            fn_type,
            implicit_self,
            checked_args,
        } = match self.resolve_callee(&call.callee, &call.args)? {
            CalleeResolution::Dynamic(result) => return result,
            CalleeResolution::Ordinary(resolved) => resolved,
        };

        let mut args = Vec::with_capacity(call.args.len() + implicit_self.is_some() as usize);
        args.extend(implicit_self);

        match checked_args {
            Some(overload_args) => args.extend(overload_args),
            None => {
                let implicit_count = args.len();

                for arg in &call.args {
                    let param_index = args.len();
                    if param_index >= fn_type.params.len() && !fn_type.is_variadic {
                        self.error(
                            arg.id,
                            arg.span,
                            AnalysisErrorKind::WrongArgumentCount {
                                expected: fn_type.params.len() - implicit_count,
                                found: call.args.len(),
                            },
                        );
                        return None;
                    }

                    let expected_type = (param_index < fn_type.params.len())
                        .then(|| &fn_type.params[param_index].1);
                    let checked_arg = self.analyze_expr(arg, expected_type)?;
                    let checked_arg = self.coerce_to_expected(expected_type, checked_arg);

                    if let Some(expected_type) = expected_type
                        && !expected_type.accepts(&checked_arg.r#type)
                    {
                        self.error(
                            arg.id,
                            arg.span,
                            AnalysisErrorKind::ArgumentTypeMismatch {
                                expected: expected_type.clone(),
                                found: checked_arg.r#type.clone(),
                            },
                        );
                        return None;
                    }

                    args.push(checked_arg);
                }
            }
        }

        let return_type = *fn_type.return_type.clone();
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: return_type,
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(callee),
                fn_type,
                args,
            }),
        })
    }

    fn analyze_place_operand(
        &mut self,
        operand: &HirExprNode,
        expected: Option<&ResolvedType>,
        error_id: HirId,
        error_span: Span,
        not_a_place: AnalysisErrorKind,
    ) -> Option<(HirPlace, CheckedPlace, ResolvedType, bool)> {
        self.with_reveal_operand(operand, |this, inner| {
            let HirExpr::Place(place) = &inner.expr else {
                this.error(error_id, error_span, not_a_place);
                return None;
            };
            let (checked, r#type, mutable) =
                this.analyze_place(inner.id, inner.span, place, expected)?;
            Some((place.clone(), checked, r#type, mutable))
        })
    }

    fn analyze_assignment(
        &mut self,
        node_id: HirId,
        span: Span,
        assignment: &omega_hir::HirAssignment,
    ) -> Option<CheckedExprNode> {
        let (place, checked_target, target_type, target_mutable) = self.analyze_place_operand(
            &assignment.target,
            None,
            node_id,
            span,
            AnalysisErrorKind::AssignmentTargetNotAPlace,
        )?;
        self.require_mutable_place(node_id, span, &place.root, &checked_target, target_mutable)?;

        let checked_value = self.analyze_expr(&assignment.value, Some(&target_type))?;
        let checked_value = self.coerce_to_expected(Some(&target_type), checked_value);

        if !target_type.accepts(&checked_value.r#type) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AssignmentTypeMismatch {
                    target: target_type,
                    value: checked_value.r#type,
                },
            );
            return None;
        }

        if let CheckedExpr::Place(value_place) = &checked_value.kind
            && Self::places_provably_equal(&checked_target, value_place)
        {
            self.warn(node_id, span, AnalysisWarningKind::SelfAssignment);
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: target_type,
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: checked_target,
                value: Box::new(checked_value),
            }),
        })
    }

    fn analyze_address_of(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        mutable: bool,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        self.with_reveal_operand(base, |this, base| {
            this.analyze_address_of_inner(node_id, span, base, mutable, expected)
        })
    }

    fn analyze_address_of_inner(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        mutable: bool,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        if let HirExpr::Slice(HirSlice {
            base: slice_base,
            range,
        }) = &base.expr
        {
            return self.analyze_slice(node_id, span, slice_base, range, mutable);
        }
        if let HirExpr::ArrayLiteral(elements) = &base.expr {
            return self.analyze_const_slice(node_id, span, elements, mutable, expected);
        }
        let HirExpr::Place(place) = &base.expr else {
            self.error(node_id, span, AnalysisErrorKind::AddressOfNotAPlace);
            return None;
        };
        let (checked_place, place_type, place_mutable) =
            self.analyze_place(base.id, base.span, place, None)?;
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked_place.root {
            self.context.mark_used(decl_id);
        }
        if !mutable {
            if let CheckedPlaceRoot::Variable {
                storage: Storage::Comp,
                ..
            } = checked_place.root
            {
                let value = self.resolve_comp_place(node_id, span, &checked_place)?;
                return Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Pointer {
                        pointee: Box::new(place_type.clone()),
                        mutable: false,
                    },
                    kind: CheckedExpr::Const(ConstValue::Ref(Box::new(value))),
                });
            }
        }
        let pointee_type = if mutable {
            self.require_mutable_place(node_id, span, &place.root, &checked_place, place_mutable)?;
            if let Some((ident, origin, ..)) = self.narrowable_place(place) {
                self.context.widen_variable(&ident, origin);
            }
            place_type.widened()
        } else {
            let narrowed_shadow =
                self.narrowable_place(place)
                    .is_some_and(|(ident, origin, ..)| {
                        self.context
                            .find_variable(&ident, origin)
                            .is_some_and(|binding| binding.narrowed)
                    });
            if narrowed_shadow {
                place_type.widened()
            } else {
                place_type
            }
        };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Pointer {
                pointee: Box::new(pointee_type),
                mutable,
            },
            kind: CheckedExpr::AddressOf(CheckedAddressOf {
                place: checked_place,
            }),
        })
    }

    pub(super) fn block_type(block: &CheckedBlock) -> Option<ResolvedType> {
        match &block.tail {
            Some(tail) if Self::expr_diverges(tail) => None,
            Some(tail) => Some(tail.r#type.clone()),
            None => match block.stmts.last() {
                Some(stmt) if Self::stmt_diverges(stmt) => None,
                _ => Some(ResolvedType::Void),
            },
        }
    }
}
