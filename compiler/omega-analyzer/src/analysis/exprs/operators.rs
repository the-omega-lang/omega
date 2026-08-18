use super::*;

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_negate(
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
                AnalysisErrorKind::CharArithmeticNotAllowed {
                    op: "-".to_string(),
                },
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

    pub(super) fn analyze_not(
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
            id: self.resolver.fresh_synthetic_id(),
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

    pub(super) fn analyze_logical(
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

        let literal_id = self.resolver.fresh_synthetic_id();
        let literal = |value: bool| CheckedBlock {
            stmts: Vec::new(),
            tail: Some(Box::new(CheckedExprNode {
                id: literal_id,
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

    pub(super) fn analyze_bit_not(
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
                AnalysisErrorKind::CharArithmeticNotAllowed {
                    op: "~".to_string(),
                },
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

    pub(super) fn analyze_binary_expr(
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

    pub(super) fn analyze_cast(
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
                let slot_offset =
                    if target_spec_id == base_spec.borrow().id && *type_args == *base_type_args {
                        0
                    } else {
                        let Some(slot_offset) = flattened.iter().position(|f| {
                            f.spec_id == target_spec_id && f.type_args() == *type_args
                        }) else {
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
            let (Some(source_class), Some(target_class)) = (
                checked_base.r#type.cast_class(self.target.pointer_bits()),
                target_type.cast_class(self.target.pointer_bits()),
            ) else {
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

    pub(super) fn analyze_incr_decr(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        op: BinaryOp,
    ) -> Option<CheckedExprNode> {
        let (place, checked_place, place_type, mutable) = self.analyze_place_operand(
            base,
            None,
            node_id,
            span,
            AnalysisErrorKind::IncrementTargetNotAPlace,
        )?;
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
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Number(one),
        };
        let place_read = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let sum = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
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
            let is_valid = operand
                .r#type
                .numeric_kind(self.target.pointer_bits())
                .is_some()
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

    fn coerce_for_binary_op(&mut self, _op: BinaryOp, operand: CheckedExprNode) -> CheckedExprNode {
        match operand.r#type.arithmetic_repr() {
            Some(repr) => self.coerce_to(operand, repr),
            None => operand,
        }
    }

    fn coerce_for_unary_op(&mut self, operand: CheckedExprNode) -> CheckedExprNode {
        match operand.r#type.arithmetic_repr() {
            Some(repr) => self.coerce_to(operand, repr),
            None => operand,
        }
    }

    fn coerce_to(&mut self, operand: CheckedExprNode, repr: ResolvedType) -> CheckedExprNode {
        let pointer_bits = self.target.pointer_bits();
        let source_class = operand
            .r#type
            .cast_class(pointer_bits)
            .expect("arithmetic_repr's source always has a cast_class");
        let target_class = repr
            .cast_class(pointer_bits)
            .expect("an arithmetic_repr target is always numeric");
        let kind = Self::resolve_cast_kind(source_class, target_class);
        let id = operand.id;
        let span = operand.span;
        let mut base = operand;
        base.id = self.resolver.fresh_synthetic_id();
        CheckedExprNode {
            id,
            span,
            r#type: repr.clone(),
            kind: CheckedExpr::Cast(CheckedCast {
                kind,
                target_type: repr,
                base: Box::new(base),
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

    pub(super) fn analyze_compound_assign(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &HirExprNode,
        op: BinaryOp,
        value: &HirExprNode,
    ) -> Option<CheckedExprNode> {
        let (place, checked_place, place_type, mutable) = self.analyze_place_operand(
            target,
            None,
            node_id,
            span,
            AnalysisErrorKind::CompoundAssignTargetNotAPlace,
        )?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let checked_value = self.analyze_expr(value, Some(&place_type))?;
        let place_read = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let combined_id = self.resolver.fresh_synthetic_id();
        let combined = self.analyze_binary_op(combined_id, span, op, place_read, checked_value)?;

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
}
