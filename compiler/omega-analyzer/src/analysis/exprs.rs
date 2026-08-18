use super::*;

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
            HirExpr::Match(m) => self.analyze_match(id, span, m),
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
        let (checked_place, r#type, _mutable) = self.analyze_place(id, span, place, expected)?;
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked_place.root {
            self.context.mark_used(decl_id);
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
        self.reveal_stack.push(false);
        let result = self.analyze_expr(inner, expected);
        let load_bearing = self.reveal_stack.pop().expect("just pushed above");
        if !load_bearing {
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

    fn analyze_assignment(
        &mut self,
        node_id: HirId,
        span: Span,
        assignment: &omega_hir::HirAssignment,
    ) -> Option<CheckedExprNode> {
        let (was_reveal, target) = Self::strip_reveal(&assignment.target);
        let HirExpr::Place(place) = &target.expr else {
            self.error(node_id, span, AnalysisErrorKind::AssignmentTargetNotAPlace);
            return None;
        };
        // `was_reveal` activates the bypass for `analyze_place`'s own
        // field-visibility checks -- `reveal` on an assignment's target
        // (`reveal a.b = c;`) wraps only `target`, so it never reaches
        // `analyze_expr`'s own `HirExpr::Reveal` arm.
        let (checked_target, target_type, target_mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(target.id, target.span, place, None)
            })?;
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
        let (was_reveal, base) = Self::strip_reveal(base);
        // `&base[range]`/`&mut base[range]` -- a slice, not an ordinary
        // pointer; see `analyze_slice`. This and the compile-time-slice form
        // below both run under `was_reveal` too, same as the plain-place
        // form further down -- the bypass has to apply at every operand
        // position, not just the one reaching `analyze_place` directly.
        if let HirExpr::Slice(HirSlice {
            base: slice_base,
            range,
        }) = &base.expr
        {
            return self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_slice(node_id, span, slice_base, range, mutable)
            });
        }
        if let HirExpr::ArrayLiteral(elements) = &base.expr {
            return self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_const_slice(node_id, span, elements, mutable, expected)
            });
        }
        let HirExpr::Place(place) = &base.expr else {
            self.error(node_id, span, AnalysisErrorKind::AddressOfNotAPlace);
            return None;
        };
        let (checked_place, place_type, place_mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(base.id, base.span, place, None)
            })?;
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
                        pointee: Box::new(place_type),
                        mutable: false,
                    },
                    kind: CheckedExpr::Const(ConstValue::Ref(Box::new(value))),
                });
            }
        }

        let pointee_type = if mutable {
            self.require_mutable_place(node_id, span, &place.root, &checked_place, place_mutable)?;
            // De-assumption: a writable alias now exists, so any later
            // direct read of this place can no longer trust a narrower type.
            if let Some((ident, origin, ..)) = self.narrowable_place(place) {
                self.context.widen_variable(&ident, origin);
            }
            place_type.widened()
        } else {
            let narrowed_shadow = self.narrowable_place(place).is_some_and(|(ident, origin, ..)| {
                self.context
                    .find_variable(&ident, origin)
                    .is_some_and(|b| b.narrowed)
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

    fn analyze_negate(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let checked_base = self.analyze_expr(base, expected)?;
        if checked_base.r#type == ResolvedType::Char {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CharArithmeticNotAllowed { op: "-".to_string() },
            );
            return None;
        }
        // Signed ints and floats only -- unary `-` on an unsigned integer is
        // rejected rather than silently wrapping.
        let negatable = matches!(
            checked_base.r#type.numeric_kind(self.target.pointer_bits()),
            Some(NumericKind::Signed(_)) | Some(NumericKind::Float(_))
        );
        if !negatable {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::InvalidNegateOperand {
                    r#type: checked_base.r#type,
                },
            );
            return None;
        }

        let r#type = checked_base.r#type.clone();
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type,
            kind: CheckedExpr::Negate(Box::new(checked_base)),
        })
    }

    fn analyze_not(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
    ) -> Option<CheckedExprNode> {
        let checked_base = self.analyze_expr(base, Some(&ResolvedType::Bool))?;
        if checked_base.r#type != ResolvedType::Bool {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::InvalidNotOperand {
                    r#type: checked_base.r#type,
                },
            );
            return None;
        }
        let truth = CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::Bool(true),
        };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op: BinaryOp::BitXor,
                left: Box::new(checked_base),
                right: Box::new(truth),
            }),
        })
    }

    fn analyze_logical(
        &mut self,
        node_id: HirId,
        span: Span,
        logical: &omega_hir::HirLogical,
    ) -> Option<CheckedExprNode> {
        let op = match logical.op {
            LogicalOp::And => "&&",
            LogicalOp::Or => "||",
        };
        let operand = |this: &mut Self, side: &HirExprNode| {
            let checked = this.analyze_expr(side, Some(&ResolvedType::Bool))?;
            if checked.r#type != ResolvedType::Bool {
                this.error(
                    side.id,
                    side.span,
                    AnalysisErrorKind::InvalidLogicalOperand {
                        op,
                        r#type: checked.r#type.clone(),
                    },
                );
                return None;
            }
            Some(checked)
        };
        let left = operand(self, &logical.left);
        let right = operand(self, &logical.right);
        let (left, right) = (left?, right?);

        let literal = |value: bool| CheckedBlock {
            stmts: Vec::new(),
            tail: Some(Box::new(CheckedExprNode {
                id: node_id,
                span,
                r#type: ResolvedType::Bool,
                kind: CheckedExpr::Bool(value),
            })),
        };
        let carry = |expr: CheckedExprNode| CheckedBlock {
            stmts: Vec::new(),
            tail: Some(Box::new(expr)),
        };
        let (then_branch, else_branch) = match logical.op {
            LogicalOp::And => (carry(right), literal(false)),
            LogicalOp::Or => (literal(true), carry(right)),
        };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::If(CheckedIf {
                branches: vec![(left, then_branch)],
                else_branch: Some(else_branch),
            }),
        })
    }

    fn analyze_bit_not(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let checked_base = self.analyze_expr(base, expected)?;
        if checked_base.r#type == ResolvedType::Char {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CharArithmeticNotAllowed { op: "~".to_string() },
            );
            return None;
        }
        let checked_base = self.coerce_for_unary_op(checked_base);
        let bitnotable = matches!(
            checked_base.r#type.numeric_kind(self.target.pointer_bits()),
            Some(NumericKind::Signed(_) | NumericKind::Unsigned(_))
        );
        if !bitnotable {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::InvalidBitNotOperand {
                    r#type: checked_base.r#type,
                },
            );
            return None;
        }

        let r#type = checked_base.r#type.clone();
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type,
            kind: CheckedExpr::BitNot(Box::new(checked_base)),
        })
    }

    fn analyze_binary_expr(
        &mut self,
        node_id: HirId,
        span: Span,
        bin: &omega_hir::HirBinaryOp,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let operand_expected = if bin.op.is_comparison() {
            None
        } else {
            expected
        };
        let checked_left = self.analyze_expr(&bin.left, operand_expected)?;
        // For a non-comparison op, anchor to what `left` will *become*
        // (`arithmetic_repr`), not what it currently is -- otherwise
        // `some_char + 1` fails to compile, since the bare `1` would anchor
        // to `char` (falling back to `i32`) while `left` coerces to `u32`
        // below, and the two would then mismatch.
        let mut left_type = checked_left.r#type.widened();
        if !bin.op.is_comparison() {
            left_type = left_type.arithmetic_repr().unwrap_or(left_type);
        }
        let checked_right = self.analyze_expr(&bin.right, operand_expected.or(Some(&left_type)))?;
        self.analyze_binary_op(node_id, span, bin.op, checked_left, checked_right)
    }

    fn analyze_cast(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &Type,
        base: &HirExprNode,
    ) -> Option<CheckedExprNode> {
        let target_type = self.resolve_type_or_error(node_id, span, target, true)?;
        let checked_base = self.analyze_expr(base, None)?;

        // Generalized over `Pointer`/`Slice`/`Str` alike, and checked before
        // either cast-kind path below, so e.g. `<*mut str>` on an immutable
        // `*str` is caught here rather than silently succeeding as a
        // `Reinterpret`.
        if target_type.pointer_like_mutable() == Some(true)
            && checked_base.r#type.pointer_like_mutable() == Some(false)
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CastToMutablePointer {
                    from: checked_base.r#type.clone(),
                    to: target_type.clone(),
                },
            );
            return None;
        }

        if let ResolvedType::SpecObject {
            spec,
            type_args,
            mutable,
        } = &target_type
        {
            if let ResolvedType::SpecObject {
                spec: base_spec,
                type_args: base_type_args,
                mutable: base_mutable,
            } = &checked_base.r#type
            {
                if *mutable && !*base_mutable {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::CastToMutablePointer {
                            from: checked_base.r#type.clone(),
                            to: target_type.clone(),
                        },
                    );
                    return None;
                }
                let flattened = self.flatten_spec(
                    node_id,
                    span,
                    base_spec,
                    base_type_args,
                    &ResolvedType::Void,
                )?;
                // The section offset is the target's slot position in the
                // source object's flattened list -- the same ordered list
                // the vtable was built from.
                let target_spec_id = spec.borrow().id;
                // Same spec, same instantiation: identity cast, offset zero.
                // Checked first because an alias's own id never appears
                // among its flattened members' entries.
                let slot_offset = if target_spec_id == base_spec.borrow().id
                    && *type_args == *base_type_args
                {
                    0
                } else {
                    let Some(slot_offset) = flattened
                        .iter()
                        .position(|f| f.spec_id == target_spec_id && f.type_args() == *type_args)
                    else {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::SpecObjectCastImpossible {
                                from: base_spec.borrow().name.clone(),
                                to: spec.borrow().name.clone(),
                            },
                        );
                        return None;
                    };
                    slot_offset
                };
                return Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: target_type.clone(),
                    kind: CheckedExpr::Cast(CheckedCast {
                        kind: CastKind::SpecNarrow { slot_offset },
                        target_type,
                        base: Box::new(checked_base),
                    }),
                });
            }
            let ResolvedType::Pointer {
                pointee,
                mutable: base_mutable,
            } = &checked_base.r#type
            else {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidCast {
                        from: checked_base.r#type.clone(),
                        to: target_type.clone(),
                    },
                );
                return None;
            };
            if *mutable && !base_mutable {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::CastToMutablePointer {
                        from: checked_base.r#type.clone(),
                        to: target_type.clone(),
                    },
                );
                return None;
            }
            let pointee = (**pointee).clone();
            let spec = spec.clone();
            let type_args = type_args.clone();
            let Ok(slots) =
                self.type_implements_spec(node_id, span, &pointee, &spec, &type_args, true)
            else {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidCast {
                        from: checked_base.r#type.clone(),
                        to: target_type.clone(),
                    },
                );
                return None;
            };
            return Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: target_type.clone(),
                kind: CheckedExpr::SpecCoerce(CheckedSpecCoerce {
                    base: Box::new(checked_base),
                    slots,
                }),
            });
        }

        let cast_kind = if let Some(kind) =
            Self::byte_pointer_cast_kind(&checked_base.r#type, &target_type)
        {
            kind
        } else if let Some(kind) = Self::unsize_cast_kind(&checked_base.r#type, &target_type) {
            kind
        } else if let Some(kind) = Self::array_pointer_cast_kind(&checked_base.r#type, &target_type)
        {
            kind
        } else {
            let (Some(source_class), Some(target_class)) =
                (checked_base.r#type.cast_class(self.target.pointer_bits()), target_type.cast_class(self.target.pointer_bits()))
            else {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidCast {
                        from: checked_base.r#type.clone(),
                        to: target_type.clone(),
                    },
                );
                return None;
            };
            if !Self::allows_cast_into(&checked_base.r#type, &target_type) {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidCast {
                        from: checked_base.r#type.clone(),
                        to: target_type.clone(),
                    },
                );
                return None;
            }
            Self::resolve_cast_kind(source_class, target_class)
        };
        if cast_kind == CastKind::Reinterpret && checked_base.r#type == target_type {
            self.warn(
                node_id,
                span,
                AnalysisWarningKind::NoOpCast {
                    r#type: target_type.clone(),
                },
            );
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: target_type.clone(),
            kind: CheckedExpr::Cast(CheckedCast {
                kind: cast_kind,
                target_type,
                base: Box::new(checked_base),
            }),
        })
    }

    fn analyze_incr_decr(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        op: BinaryOp,
    ) -> Option<CheckedExprNode> {
        let (was_reveal, base) = Self::strip_reveal(base);
        let HirExpr::Place(place) = &base.expr else {
            self.error(node_id, span, AnalysisErrorKind::IncrementTargetNotAPlace);
            return None;
        };
        let (checked_place, place_type, mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(base.id, base.span, place, None)
            })?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let Some(kind) = place_type.numeric_kind(self.target.pointer_bits()) else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::InvalidIncrementOperand { r#type: place_type },
            );
            return None;
        };

        let one = match kind {
            NumericKind::Signed(_) => NumberValue::Signed(1),
            NumericKind::Unsigned(_) => NumberValue::Unsigned(1),
            NumericKind::Float(_) => NumberValue::Float(1.0),
        };
        let one_node = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Number(one),
        };
        let place_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let sum = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op,
                left: Box::new(place_read),
                right: Box::new(one_node),
            }),
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type,
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: checked_place,
                value: Box::new(sum),
            }),
        })
    }

    fn analyze_binary_op(
        &mut self,
        node_id: HirId,
        span: Span,
        op: BinaryOp,
        checked_left: CheckedExprNode,
        checked_right: CheckedExprNode,
    ) -> Option<CheckedExprNode> {
        if !op.is_comparison()
            && (checked_left.r#type == ResolvedType::Char
                || checked_right.r#type == ResolvedType::Char)
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CharArithmeticNotAllowed {
                    op: op.symbol().to_string(),
                },
            );
            return None;
        }
        if matches!(checked_left.r#type, ResolvedType::Pointer { .. })
            && matches!(checked_right.r#type, ResolvedType::Pointer { .. })
            && !op.is_comparison()
            && op != BinaryOp::Sub
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::PointerPairArithmetic { op },
            );
            return None;
        }

        let checked_left = self.coerce_for_binary_op(op, checked_left);
        let checked_right = self.coerce_for_binary_op(op, checked_right);

        for operand in [&checked_left, &checked_right] {
            let is_valid = operand.r#type.numeric_kind(self.target.pointer_bits()).is_some()
                || (op.is_comparison() && operand.r#type == ResolvedType::Char)
                || (operand.r#type == ResolvedType::Bool
                    && matches!(
                        op,
                        BinaryOp::Eq
                            | BinaryOp::Ne
                            | BinaryOp::BitAnd
                            | BinaryOp::BitOr
                            | BinaryOp::BitXor
                    ));
            if !is_valid {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidBinaryOperand {
                        op,
                        r#type: operand.r#type.clone(),
                    },
                );
                return None;
            }
        }

        if checked_left.r#type != checked_right.r#type {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::BinaryOperandTypeMismatch {
                    left: checked_left.r#type.clone(),
                    left_span: checked_left.span,
                    right: checked_right.r#type.clone(),
                    right_span: checked_right.span,
                },
            );
            return None;
        }

        if op == BinaryOp::Rem
            && matches!(
                checked_left.r#type.numeric_kind(self.target.pointer_bits()),
                Some(NumericKind::Float(_))
            )
        {
            self.error(node_id, span, AnalysisErrorKind::FloatRemainder);
            return None;
        }

        if matches!(
            op,
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr
        ) && matches!(
            checked_left.r#type.numeric_kind(self.target.pointer_bits()),
            Some(NumericKind::Float(_))
        ) {
            self.error(node_id, span, AnalysisErrorKind::FloatBitwiseOperand);
            return None;
        }

        if op.is_comparison() {
            self.check_always_true_false_comparison(
                node_id,
                span,
                op,
                &checked_left,
                &checked_right,
            );
        }

        let r#type = if op.is_comparison() {
            ResolvedType::Bool
        } else {
            checked_left.r#type.clone()
        };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op,
                left: Box::new(checked_left),
                right: Box::new(checked_right),
            }),
        })
    }

    fn coerce_for_binary_op(&self, _op: BinaryOp, operand: CheckedExprNode) -> CheckedExprNode {
        match operand.r#type.arithmetic_repr() {
            Some(repr) => Self::coerce_to(operand, repr, self.target.pointer_bits()),
            None => operand,
        }
    }

    fn coerce_for_unary_op(&self, operand: CheckedExprNode) -> CheckedExprNode {
        match operand.r#type.arithmetic_repr() {
            Some(repr) => Self::coerce_to(operand, repr, self.target.pointer_bits()),
            None => operand,
        }
    }

    fn coerce_to(operand: CheckedExprNode, repr: ResolvedType, pointer_bits: u32) -> CheckedExprNode {
        let source_class = operand
            .r#type
            .cast_class(pointer_bits)
            .expect("arithmetic_repr's source always has a cast_class");
        let target_class = repr
            .cast_class(pointer_bits)
            .expect("an arithmetic_repr target is always numeric");
        let kind = Self::resolve_cast_kind(source_class, target_class);
        CheckedExprNode {
            id: operand.id,
            span: operand.span,
            r#type: repr.clone(),
            kind: CheckedExpr::Cast(CheckedCast {
                kind,
                target_type: repr,
                base: Box::new(operand),
            }),
        }
    }

    fn literal_i128(expr: &CheckedExprNode) -> Option<i128> {
        match &expr.kind {
            CheckedExpr::Number(NumberValue::Signed(n)) => Some(*n as i128),
            CheckedExpr::Number(NumberValue::Unsigned(n)) => Some(*n as i128),
            CheckedExpr::Bool(b) => Some(*b as i128),
            CheckedExpr::Char(c) => Some(*c as i128),
            _ => None,
        }
    }

    fn check_always_true_false_comparison(
        &mut self,
        node_id: HirId,
        span: Span,
        op: BinaryOp,
        left: &CheckedExprNode,
        right: &CheckedExprNode,
    ) {
        let Some((lo, hi)) = left.r#type.integer_domain(self.target.pointer_bits()) else {
            return;
        };

        let (literal, literal_on_right) =
            match (Self::literal_i128(left), Self::literal_i128(right)) {
                (Some(l), None) => (l, false),
                (None, Some(r)) => (r, true),
                _ => return,
            };

        let fixed = if literal_on_right {
            match op {
                BinaryOp::Lt => (hi < literal)
                    .then_some(true)
                    .or((lo >= literal).then_some(false)),
                BinaryOp::Le => (hi <= literal)
                    .then_some(true)
                    .or((lo > literal).then_some(false)),
                BinaryOp::Gt => (lo > literal)
                    .then_some(true)
                    .or((hi <= literal).then_some(false)),
                BinaryOp::Ge => (lo >= literal)
                    .then_some(true)
                    .or((hi < literal).then_some(false)),
                BinaryOp::Eq => (literal < lo || literal > hi).then_some(false),
                BinaryOp::Ne => (literal < lo || literal > hi).then_some(true),
                _ => None,
            }
        } else {
            match op {
                BinaryOp::Lt => (literal < lo)
                    .then_some(true)
                    .or((literal >= hi).then_some(false)),
                BinaryOp::Le => (literal <= lo)
                    .then_some(true)
                    .or((literal > hi).then_some(false)),
                BinaryOp::Gt => (literal > hi)
                    .then_some(true)
                    .or((literal <= lo).then_some(false)),
                BinaryOp::Ge => (literal >= hi)
                    .then_some(true)
                    .or((literal < lo).then_some(false)),
                BinaryOp::Eq => (literal < lo || literal > hi).then_some(false),
                BinaryOp::Ne => (literal < lo || literal > hi).then_some(true),
                _ => None,
            }
        };

        if let Some(result) = fixed {
            self.warn(
                node_id,
                span,
                AnalysisWarningKind::AlwaysTrueFalseComparison { result },
            );
        }
    }

    fn analyze_compound_assign(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &HirExprNode,
        op: BinaryOp,
        value: &HirExprNode,
    ) -> Option<CheckedExprNode> {
        let (was_reveal, target) = Self::strip_reveal(target);
        let HirExpr::Place(place) = &target.expr else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CompoundAssignTargetNotAPlace,
            );
            return None;
        };
        let (checked_place, place_type, mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(target.id, target.span, place, None)
            })?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let checked_value = self.analyze_expr(value, Some(&place_type))?;
        let place_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let combined = self.analyze_binary_op(node_id, span, op, place_read, checked_value)?;

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type,
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: checked_place,
                value: Box::new(combined),
            }),
        })
    }

    fn allows_cast_into(source: &ResolvedType, target: &ResolvedType) -> bool {
        match target {
            ResolvedType::Char => matches!(source, ResolvedType::Char | ResolvedType::U8),
            ResolvedType::Bool => *source == ResolvedType::Bool,
            _ => true,
        }
    }

    fn resolve_cast_kind(source: CastClass, target: CastClass) -> CastKind {
        match (source, target) {
            (CastClass::Int { width: sw, signed }, CastClass::Int { width: tw, .. }) => {
                if sw == tw {
                    CastKind::Reinterpret
                } else if sw < tw {
                    CastKind::IntExtend { signed }
                } else {
                    CastKind::IntTruncate
                }
            }
            (CastClass::Int { signed, .. }, CastClass::Float { .. }) => {
                CastKind::IntToFloat { signed }
            }
            (CastClass::Float { .. }, CastClass::Int { signed, .. }) => {
                CastKind::FloatToInt { signed }
            }
            (CastClass::Float { width: sw }, CastClass::Float { width: tw }) => {
                if sw == tw {
                    CastKind::Reinterpret
                } else if sw < tw {
                    CastKind::FloatExtend
                } else {
                    CastKind::FloatTruncate
                }
            }
        }
    }

    fn byte_pointer_cast_kind(source: &ResolvedType, target: &ResolvedType) -> Option<CastKind> {
        fn is_byte_run(t: &ResolvedType) -> bool {
            matches!(t, ResolvedType::Str { .. })
                || matches!(t, ResolvedType::Slice { item, .. } if matches!(**item, ResolvedType::U8 | ResolvedType::I8))
        }
        if !is_byte_run(source) {
            return None;
        }
        if is_byte_run(target) {
            return Some(CastKind::Reinterpret);
        }
        if matches!(target, ResolvedType::Pointer { pointee, .. } if matches!(**pointee, ResolvedType::U8 | ResolvedType::I8))
        {
            return Some(CastKind::DropLength);
        }
        None
    }

    fn unsize_cast_kind(source: &ResolvedType, target: &ResolvedType) -> Option<CastKind> {
        let ResolvedType::Pointer { pointee, .. } = source else {
            return None;
        };
        let ResolvedType::SizedArray(item, _) = pointee.as_ref() else {
            return None;
        };
        let ResolvedType::Slice {
            item: target_item, ..
        } = target
        else {
            return None;
        };
        (item.as_ref() == target_item.as_ref()).then_some(CastKind::Unsize)
    }

    fn array_pointer_cast_kind(source: &ResolvedType, target: &ResolvedType) -> Option<CastKind> {
        match (source, target) {
            (ResolvedType::Pointer { .. }, ResolvedType::Array(_, _))
            | (ResolvedType::Array(_, _), ResolvedType::Pointer { .. }) => {
                Some(CastKind::Reinterpret)
            }
            _ => None,
        }
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

    fn analyze_range_value(
        &mut self,
        id: HirId,
        span: Span,
        range: &HirRange,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // Each written bound is analyzed exactly once. The end adapts to the
        // start's type when there is one, so `lo..=255` gives the literal
        // `lo`'s type rather than letting it default to `i32` independently.
        let checked_start = match &range.start {
            Some(expr) => Some(self.analyze_expr(expr, None)?),
            None => None,
        };
        let checked_end = match (range.end.expr(), &checked_start) {
            (Some(expr), Some(start)) => Some(self.analyze_expr(expr, Some(&start.r#type))?),
            (Some(expr), None) => Some(self.analyze_expr(expr, None)?),
            (None, _) => None,
        };

        let element = match (&checked_start, &checked_end) {
            (Some(start), _) => start.r#type.clone(),
            (None, Some(end)) => end.r#type.clone(),
            (None, None) => match Self::expected_range_element(expected) {
                Some(element) => element,
                None => {
                    self.error(id, span, AnalysisErrorKind::RangeNotAllowedHere);
                    return None;
                }
            },
        };

        let start = match checked_start {
            Some(value) => self.coerce_to_expected(Some(&element), value),
            None => self.synthesize_bounded_call(id, span, &element, "min")?,
        };
        let end = match checked_end {
            Some(value) => self.coerce_to_expected(Some(&element), value),
            None => self.synthesize_bounded_call(id, span, &element, "max")?,
        };

        let ResolvedItem::Type(ResolvedType::Struct(cell)) = self
            .resolve_item_checked(&Self::core_range_path("Range"), &[element], true)
            .ok()?
        else {
            return None;
        };
        // Field indices, not names: `runtime/core/range.omg` declares
        // `start`, `end`, `inclusive` in exactly this order. Reordering them
        // there without changing these silently builds the wrong range.
        Some(CheckedExprNode {
            id,
            span,
            r#type: ResolvedType::Struct(cell),
            kind: CheckedExpr::StructLiteral(CheckedStructLiteral {
                fields: vec![
                    CheckedStructLiteralField {
                        field_index: 0,
                        value: start,
                    },
                    CheckedStructLiteralField {
                        field_index: 1,
                        value: end,
                    },
                    CheckedStructLiteralField {
                        field_index: 2,
                        value: CheckedExprNode {
                            id,
                            span,
                            r#type: ResolvedType::Bool,
                            kind: CheckedExpr::Bool(range.inclusive()),
                        },
                    },
                ],
            }),
        })
    }

    fn core_range_path(name: &str) -> Vec<Ident> {
        vec![
            Ident("core".to_string()),
            Ident("range".to_string()),
            Ident(name.to_string()),
        ]
    }

    fn expected_range_element(expected: Option<&ResolvedType>) -> Option<ResolvedType> {
        let ResolvedType::Struct(cell) = expected? else {
            return None;
        };
        let definition = cell.borrow();
        let is_core_range = definition.name.as_ref() == "Range"
            && definition.module_path.len() == 2
            && definition.module_path[0].as_ref() == "core"
            && definition.module_path[1].as_ref() == "range";
        is_core_range
            .then(|| definition.type_args.first().cloned())
            .flatten()
    }

    fn synthesize_bounded_call(
        &mut self,
        id: HirId,
        span: Span,
        target: &ResolvedType,
        name: &str,
    ) -> Option<CheckedExprNode> {
        let ResolvedItem::Type(ResolvedType::Spec(spec)) = self
            .resolve_item_checked(&Self::core_range_path("Bounded"), &[], true)
            .ok()?
        else {
            return None;
        };
        let Some(conform) = self
            .resolver
            .conformance_for(target, &spec, &[])
            .ok()
            .flatten()
        else {
            self.error(
                id,
                span,
                AnalysisErrorKind::RangeNeedsBounded {
                    r#type: target.clone(),
                },
            );
            return None;
        };
        let method = conform
            .methods
            .into_iter()
            .find(|(method_name, method)| {
                method_name.as_ref() == name && method.fn_type.self_mode.is_none()
            })
            .map(|(_, method)| method)?;
        let fn_type = method.fn_type.clone();
        let function = ResolvedType::Function(fn_type.clone());
        Some(CheckedExprNode {
            id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(CheckedExprNode {
                    id,
                    span,
                    r#type: function.clone(),
                    kind: CheckedExpr::Place(CheckedPlace {
                        root: CheckedPlaceRoot::Variable {
                            decl_id: method.decl_id,
                            storage: Storage::Function,
                            r#type: function.clone(),
                        },
                        projections: vec![],
                        r#type: function,
                    }),
                }),
                fn_type,
                args: vec![],
            }),
        })
    }
}
