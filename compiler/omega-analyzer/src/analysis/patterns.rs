use super::*;

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
    ) -> Option<CheckedExprNode> {
        let narrow_target = self.narrowable_scrutinee(&m.scrutinee);
        let checked_scrutinee = self.analyze_expr(&m.scrutinee, None)?;
        let scrutinee_type = checked_scrutinee.r#type.clone();

        let (scrutinee_place, prelude_stmts, narrow_binding) =
            if let Some((ident, origin, decl_id, storage, mutable)) = narrow_target {
                let CheckedExpr::Place(place) = checked_scrutinee.kind else {
                    unreachable!("a narrowable scrutinee is always analyzed as a place")
                };
                (
                    place,
                    Vec::new(),
                    Some((ident, origin, decl_id, storage, mutable)),
                )
            } else {
                let temporary_id = self.resolver.fresh_synthetic_id();
                let target = CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: temporary_id,
                        storage: Storage::Local,
                        r#type: scrutinee_type.clone(),
                    },
                    projections: vec![],
                    r#type: scrutinee_type.clone(),
                };
                let decl = CheckedStmt::Declaration(CheckedDeclaration {
                    id: temporary_id,
                    span,
                    ident: Ident("$scrutinee".to_string()),
                    r#type: scrutinee_type.clone(),
                    mutable: true,
                    initial_value: None,
                });
                let assign = CheckedStmt::Expression(CheckedExprNode {
                    id: self.resolver.fresh_synthetic_id(),
                    span,
                    r#type: scrutinee_type.clone(),
                    kind: CheckedExpr::Assignment(CheckedAssignment {
                        target: target.clone(),
                        value: Box::new(checked_scrutinee),
                    }),
                });
                (target, vec![decl, assign], None)
            };

        let matched = Self::matched_through_pointer(&scrutinee_type);

        let (arms, else_branch, result_type) = if matches!(matched, ResolvedType::Enum { .. }) {
            self.analyze_enum_match(
                node_id,
                span,
                m,
                &scrutinee_type,
                &scrutinee_place,
                narrow_binding,
            )?
        } else if let ResolvedType::AnonymousEnum { shape, .. } = matched {
            let shape = shape.clone();
            self.analyze_anonymous_enum_match(
                node_id,
                span,
                m,
                &shape,
                &scrutinee_type,
                &scrutinee_place,
                narrow_binding,
            )?
        } else if scrutinee_type
            .integer_domain(self.target.pointer_bits())
            .is_some()
        {
            self.analyze_value_match(node_id, span, m, &scrutinee_type, &scrutinee_place)?
        } else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::UnsupportedMatchScrutinee {
                    r#type: scrutinee_type,
                },
            );
            return None;
        };

        if prelude_stmts.is_empty() {
            Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: result_type,
                kind: CheckedExpr::Match(CheckedMatch { arms, else_branch }),
            })
        } else {
            let checked_match = CheckedExprNode {
                id: self.resolver.fresh_synthetic_id(),
                span,
                r#type: result_type.clone(),
                kind: CheckedExpr::Match(CheckedMatch { arms, else_branch }),
            };
            Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: result_type,
                kind: CheckedExpr::Codeblock(CheckedBlock {
                    stmts: prelude_stmts,
                    tail: Some(Box::new(checked_match)),
                }),
            })
        }
    }

    pub(super) fn narrowable_place(
        &self,
        place: &HirPlace,
    ) -> Option<(Ident, Origin, HirId, Storage, bool)> {
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else {
            return None;
        };
        if !expr_path.generic_args.is_empty() || !expr_path.path.is_unqualified() {
            return None;
        }
        let origin = expr_path.path.origin;
        let binding = self.context.find_variable(&expr_path.path.head, origin)?;
        Some((
            expr_path.path.head.clone(),
            origin,
            binding.decl_id,
            binding.storage,
            binding.mutable,
        ))
    }

    fn narrowable_scrutinee(
        &self,
        scrutinee: &HirExprNode,
    ) -> Option<(Ident, Origin, HirId, Storage, bool)> {
        let HirExpr::Place(place) = &Self::strip_reveal(scrutinee).1.expr else {
            return None;
        };
        self.narrowable_place(place)
    }

    fn analyze_match_arm_body(&mut self, body: &HirExprNode) -> Option<CheckedBlock> {
        if let HirExpr::Codeblock(block) = &body.expr {
            self.analyze_block(block, None)
        } else {
            let checked = self.analyze_expr(body, None)?;
            Some(CheckedBlock {
                stmts: vec![],
                tail: Some(Box::new(checked)),
            })
        }
    }

    fn analyze_enum_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_place: &CheckedPlace,
        narrow_binding: Option<(Ident, Origin, HirId, Storage, bool)>,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        let (cell, through_pointer) = match scrutinee_type {
            ResolvedType::Enum { cell, .. } => (cell.clone(), None),
            ResolvedType::Pointer { pointee, mutable } => match &**pointee {
                ResolvedType::Enum { cell, .. } => (cell.clone(), Some(*mutable)),
                _ => unreachable!("caller already confirmed this is an enum or pointer-to-enum"),
            },
            _ => unreachable!("caller already confirmed this is an enum or pointer-to-enum"),
        };

        let mut tag_projections = Vec::new();
        let tag_type = self.resolve_field_projection(
            node_id,
            span,
            &mut tag_projections,
            scrutinee_type,
            &Ident("tag".to_string()),
            &mut false,
        )?;
        let tag_place = CheckedPlace {
            root: scrutinee_place.root.clone(),
            projections: tag_projections,
            r#type: tag_type.clone(),
        };

        let mut covered: HashMap<usize, Span> = HashMap::new();
        let mut checked_arms = Vec::with_capacity(m.arms.len());
        let mut catch_all: Option<&HirMatchArm> = None;
        for arm in &m.arms {
            if arm.pattern.catch_all_range().is_some() {
                if let Some(previous) = catch_all {
                    self.error(
                        node_id,
                        arm.pattern.span(),
                        AnalysisErrorKind::MultipleCatchAllPatterns {
                            previous: previous.pattern.span(),
                        },
                    );
                    return None;
                }
                catch_all = Some(arm);
                continue; // resolved separately below, once every other arm's coverage is known
            }
            let Some(HirPatternValue::Value(pattern_expr)) = &arm.pattern.value else {
                self.error(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::PatternNotEnumVariant {
                        r#enum: cell.borrow().name.clone(),
                    },
                );
                return None;
            };
            let variant_index = self.resolve_variant_pattern(&cell, pattern_expr)?;

            if let Some(previous) = covered.insert(variant_index, arm.pattern.span()) {
                self.error(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::OverlappingMatchArm { previous },
                );
                return None;
            }

            let condition = self.tag_variant_condition(
                &tag_place,
                &tag_type,
                &cell,
                variant_index,
                arm.pattern.span(),
            );

            let (body, _) = self.with_scope(|this| {
                if let Some((ident, origin, decl_id, storage, mutable)) = &narrow_binding {
                    let refined = ResolvedType::Enum {
                        cell: cell.clone(),
                        variant: Some(variant_index),
                    };
                    let narrowed = match through_pointer {
                        Some(pointer_mutable) => ResolvedType::Pointer {
                            pointee: Box::new(refined),
                            mutable: pointer_mutable,
                        },
                        None => refined,
                    };
                    this.declare_narrowed_binding(
                        *decl_id, arm.span, ident, *origin, narrowed, *storage, *mutable,
                    );
                }
                this.analyze_match_arm_body(&arm.body)
            });

            checked_arms.push(CheckedMatchArm {
                conditions: vec![vec![condition]],
                body: body?,
            });
        }

        let variant_count = cell.borrow().variants.len();
        let missing: Vec<usize> = (0..variant_count)
            .filter(|idx| !covered.contains_key(idx))
            .collect();

        if let Some(arm) = catch_all {
            if missing.is_empty() {
                self.error(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::CatchAllPatternRedundant,
                );
                return None;
            }
            let conditions = missing
                .iter()
                .map(|&idx| {
                    vec![self.tag_variant_condition(
                        &tag_place,
                        &tag_type,
                        &cell,
                        idx,
                        arm.pattern.span(),
                    )]
                })
                .collect();
            let body = self.analyze_match_arm_body(&arm.body)?;
            checked_arms.push(CheckedMatchArm { conditions, body });
        }

        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if catch_all.is_none() && !missing.is_empty() => {
                let missing_names = missing
                    .iter()
                    .map(|&idx| cell.borrow().variants[idx].name.clone())
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchEnum {
                        r#enum: cell.borrow().name.clone(),
                        missing: missing_names,
                    },
                );
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    /// The type a `match` actually discriminates on: matching through a
    /// pointer proves the pointee's variant, so both enum forms accept a
    /// direct or a single-pointer scrutinee.
    fn matched_through_pointer(scrutinee_type: &ResolvedType) -> &ResolvedType {
        match scrutinee_type {
            ResolvedType::Pointer { pointee, .. } => pointee,
            other => other,
        }
    }

    fn analyze_anonymous_enum_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        shape: &Rc<ResolvedAnonymousEnum>,
        scrutinee_type: &ResolvedType,
        scrutinee_place: &CheckedPlace,
        narrow_binding: Option<(Ident, Origin, HirId, Storage, bool)>,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        let through_pointer = match scrutinee_type {
            ResolvedType::Pointer { mutable, .. } => Some(*mutable),
            _ => None,
        };
        let parent = ResolvedType::AnonymousEnum {
            shape: shape.clone(),
            variant: None,
        };

        // An anonymous enum has no declaration and therefore no `tag` field to
        // look up by name; the projection is built from the type itself.
        let tag_type = crate::layout::ANONYMOUS_ENUM_TAG_TYPE;
        let mut tag_projections = Vec::new();
        if through_pointer.is_some() {
            tag_projections.push(CheckedProjection::Deref {
                r#type: parent.clone(),
            });
        }
        tag_projections.push(CheckedProjection::EnumTag {
            r#type: tag_type.clone(),
        });
        let tag_place = CheckedPlace {
            root: scrutinee_place.root.clone(),
            projections: scrutinee_place
                .projections
                .iter()
                .cloned()
                .chain(tag_projections)
                .collect(),
            r#type: tag_type.clone(),
        };

        let mut covered: HashMap<usize, Span> = HashMap::new();
        let mut checked_arms = Vec::with_capacity(m.arms.len());
        let mut catch_all: Option<&HirMatchArm> = None;
        for arm in &m.arms {
            if arm.pattern.catch_all_range().is_some() {
                if let Some(previous) = catch_all {
                    self.error(
                        node_id,
                        arm.pattern.span(),
                        AnalysisErrorKind::MultipleCatchAllPatterns {
                            previous: previous.pattern.span(),
                        },
                    );
                    return None;
                }
                catch_all = Some(arm);
                continue; // resolved separately below, once every other arm's coverage is known
            }
            let member_index = self.resolve_anonymous_member_pattern(node_id, arm, shape, &parent)?;

            if let Some(previous) = covered.insert(member_index, arm.pattern.span()) {
                self.error(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::OverlappingMatchArm { previous },
                );
                return None;
            }

            let condition =
                self.member_tag_condition(&tag_place, &tag_type, member_index, arm.pattern.span());

            let (body, _) = self.with_scope(|this| {
                if let Some((ident, origin, decl_id, storage, mutable)) = &narrow_binding {
                    let refined = ResolvedType::AnonymousEnum {
                        shape: shape.clone(),
                        variant: Some(member_index),
                    };
                    let narrowed = match through_pointer {
                        Some(pointer_mutable) => ResolvedType::Pointer {
                            pointee: Box::new(refined),
                            mutable: pointer_mutable,
                        },
                        None => refined,
                    };
                    this.declare_narrowed_binding(
                        *decl_id, arm.span, ident, *origin, narrowed, *storage, *mutable,
                    );
                }
                this.analyze_match_arm_body(&arm.body)
            });

            checked_arms.push(CheckedMatchArm {
                conditions: vec![vec![condition]],
                body: body?,
            });
        }

        let missing: Vec<usize> = (0..shape.members().len())
            .filter(|index| !covered.contains_key(index))
            .collect();

        if let Some(arm) = catch_all {
            if missing.is_empty() {
                self.error(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::CatchAllPatternRedundant,
                );
                return None;
            }
            let conditions = missing
                .iter()
                .map(|&index| {
                    vec![self.member_tag_condition(
                        &tag_place,
                        &tag_type,
                        index,
                        arm.pattern.span(),
                    )]
                })
                .collect();
            let body = self.analyze_match_arm_body(&arm.body)?;
            checked_arms.push(CheckedMatchArm { conditions, body });
        }

        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if catch_all.is_none() && !missing.is_empty() => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchAnonymousEnum {
                        r#enum: parent.clone(),
                        missing: missing
                            .iter()
                            .map(|&index| shape.members()[index].clone())
                            .collect(),
                    },
                );
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    /// An anonymous-enum arm names a member type. The parser kept a type
    /// reading of the pattern only when the whole pattern parsed as one, so a
    /// missing candidate here means the arm was never spelled as a type.
    fn resolve_anonymous_member_pattern(
        &mut self,
        node_id: HirId,
        arm: &HirMatchArm,
        shape: &ResolvedAnonymousEnum,
        parent: &ResolvedType,
    ) -> Option<usize> {
        let span = arm.pattern.span();
        let Some(raw_type) = &arm.pattern.r#type else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AnonymousEnumPatternNotAType {
                    r#enum: parent.clone(),
                },
            );
            return None;
        };
        let member = self.resolve_type_or_error(node_id, span, raw_type, false)?;
        match shape.index_of(&member) {
            Some(index) => Some(index),
            None => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NotAnAnonymousEnumMember {
                        found: member,
                        r#enum: parent.clone(),
                    },
                );
                None
            }
        }
    }

    fn member_tag_condition(
        &mut self,
        tag_place: &CheckedPlace,
        tag_type: &ResolvedType,
        member_index: usize,
        span: Span,
    ) -> CheckedExprNode {
        let tag_read = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Place(tag_place.clone()),
        };
        let tag_const = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Number(NumberValue::Unsigned(member_index as u64)),
        };
        CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(tag_read),
                right: Box::new(tag_const),
            }),
        }
    }

    fn tag_variant_condition(
        &mut self,
        tag_place: &CheckedPlace,
        tag_type: &ResolvedType,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        variant_index: usize,
        span: Span,
    ) -> CheckedExprNode {
        let tag_read = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Place(tag_place.clone()),
        };
        let tag_const = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Number(cell.borrow().variants[variant_index].tag),
        };
        CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(tag_read),
                right: Box::new(tag_const),
            }),
        }
    }

    fn resolve_variant_pattern(
        &mut self,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        expr: &HirExprNode,
    ) -> Option<usize> {
        let expr = Self::strip_reveal(expr).1;
        let shaped_as_variant_path = matches!(
            &expr.expr,
            HirExpr::Place(HirPlace { root: HirPlaceRoot::Path(p), projections })
                if projections.is_empty() && p.generic_args.is_empty() && !p.path.tail.is_empty()
        );
        if !shaped_as_variant_path {
            self.error(
                expr.id,
                expr.span,
                AnalysisErrorKind::PatternNotEnumVariant {
                    r#enum: cell.borrow().name.clone(),
                },
            );
            return None;
        }
        let HirExpr::Place(HirPlace {
            root: HirPlaceRoot::Path(expr_path),
            ..
        }) = &expr.expr
        else {
            unreachable!("just confirmed above")
        };
        let variant_name = expr_path
            .path
            .tail
            .last()
            .expect("just confirmed non-empty above");
        let enum_name_segment = if expr_path.path.tail.len() == 1 {
            &expr_path.path.head
        } else {
            &expr_path.path.tail[expr_path.path.tail.len() - 2]
        };
        if *enum_name_segment != cell.borrow().name {
            self.error(
                expr.id,
                expr.span,
                AnalysisErrorKind::PatternIsEnumVariant {
                    r#enum: enum_name_segment.clone(),
                    variant: variant_name.clone(),
                    scrutinee: ResolvedType::Enum {
                        cell: cell.clone(),
                        variant: None,
                    },
                },
            );
            return None;
        }
        let found = cell.borrow().variant(variant_name).map(|(idx, _)| idx);
        match found {
            Some(idx) => Some(idx),
            None => {
                let similar =
                    best_match(variant_name, cell.borrow().variants.iter().map(|v| &v.name));
                self.error(
                    expr.id,
                    expr.span,
                    AnalysisErrorKind::NoSuchVariantInPattern {
                        r#enum: cell.borrow().name.clone(),
                        name: variant_name.clone(),
                        similar,
                    },
                );
                None
            }
        }
    }

    fn analyze_value_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_place: &CheckedPlace,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        let domain = scrutinee_type
            .integer_domain(self.target.pointer_bits())
            .expect("caller already confirmed this type has an integer domain");

        let mut catch_all: Option<&HirMatchArm> = None;
        for arm in &m.arms {
            if arm.pattern.catch_all_range().is_some() {
                if let Some(previous) = catch_all {
                    self.error(
                        node_id,
                        arm.pattern.span(),
                        AnalysisErrorKind::MultipleCatchAllPatterns {
                            previous: previous.pattern.span(),
                        },
                    );
                    return None;
                }
                catch_all = Some(arm);
            }
        }

        let mut checked_arms = Vec::with_capacity(m.arms.len());
        let mut intervals = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            if arm.pattern.catch_all_range().is_some() {
                continue; // resolved separately below, once every other arm's interval is known
            }
            let (lo, hi, conditions) =
                self.analyze_value_pattern(&arm.pattern, scrutinee_type, scrutinee_place)?;
            intervals.push(crate::exhaustiveness::Interval {
                lo,
                hi,
                span: arm.pattern.span(),
            });
            let body = self.analyze_match_arm_body(&arm.body)?;
            checked_arms.push(CheckedMatchArm {
                conditions: vec![conditions],
                body,
            });
        }

        if let Some(arm) = catch_all {
            let gaps = crate::exhaustiveness::check(domain, intervals.clone()).gaps;
            let (lo, hi) = match gaps[..] {
                [] => {
                    self.error(
                        node_id,
                        arm.pattern.span(),
                        AnalysisErrorKind::CatchAllPatternRedundant,
                    );
                    return None;
                }
                [one] => one,
                _ => {
                    self.error(
                        node_id,
                        arm.pattern.span(),
                        AnalysisErrorKind::CatchAllRangeNotInferable { gaps: gaps.len() },
                    );
                    return None;
                }
            };
            let conditions = self.interval_conditions(
                scrutinee_place,
                scrutinee_type,
                domain,
                lo,
                hi,
                arm.pattern.span(),
                self.target.pointer_bits(),
            );
            intervals.push(crate::exhaustiveness::Interval {
                lo,
                hi,
                span: arm.pattern.span(),
            });
            let body = self.analyze_match_arm_body(&arm.body)?;
            checked_arms.push(CheckedMatchArm {
                conditions: vec![conditions],
                body,
            });
        }

        let coverage = crate::exhaustiveness::check(domain, intervals);
        if !coverage.overlaps.is_empty() {
            for (a, b) in &coverage.overlaps {
                let (earlier, later) = if a.span.start <= b.span.start {
                    (a.span, b.span)
                } else {
                    (b.span, a.span)
                };
                self.error(
                    node_id,
                    later,
                    AnalysisErrorKind::OverlappingMatchArm { previous: earlier },
                );
            }
            return None;
        }

        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if !coverage.gaps.is_empty() => {
                let gaps = coverage
                    .gaps
                    .iter()
                    .map(|(lo, hi)| Self::describe_gap(scrutinee_type, *lo, *hi))
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchValue {
                        r#type: scrutinee_type.clone(),
                        gaps,
                    },
                );
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    fn analyze_value_pattern(
        &mut self,
        pattern: &HirPattern,
        scrutinee_type: &ResolvedType,
        scrutinee_place: &CheckedPlace,
    ) -> Option<(i128, i128, Vec<CheckedExprNode>)> {
        match pattern.value.as_ref()? {
            HirPatternValue::Value(expr) => {
                let value = self.const_eval_pattern(expr, scrutinee_type)?;
                let n = Self::const_value_as_i128(&value);
                let condition = self.value_cmp_condition(
                    scrutinee_place,
                    expr.span,
                    scrutinee_type,
                    BinaryOp::Eq,
                    value,
                );
                Some((n, n, vec![condition]))
            }
            HirPatternValue::Range(range) => {
                let domain = scrutinee_type
                    .integer_domain(self.target.pointer_bits())
                    .expect("caller already confirmed an integer domain");
                let mut conditions = Vec::new();
                let lo = match &range.start {
                    Some(e) => {
                        let value = self.const_eval_pattern(e, scrutinee_type)?;
                        let n = Self::const_value_as_i128(&value);
                        conditions.push(self.value_cmp_condition(
                            scrutinee_place,
                            e.span,
                            scrutinee_type,
                            BinaryOp::Ge,
                            value,
                        ));
                        n
                    }
                    None => domain.0,
                };
                let hi = match range.end.expr() {
                    Some(e) => {
                        let value = self.const_eval_pattern(e, scrutinee_type)?;
                        let n = Self::const_value_as_i128(&value);
                        let inclusive = range.inclusive();
                        let op = if inclusive {
                            BinaryOp::Le
                        } else {
                            BinaryOp::Lt
                        };
                        conditions.push(self.value_cmp_condition(
                            scrutinee_place,
                            e.span,
                            scrutinee_type,
                            op,
                            value,
                        ));
                        if inclusive { n } else { n - 1 }
                    }
                    None => domain.1,
                };
                Some((lo, hi, conditions))
            }
        }
    }

    fn interval_conditions(
        &mut self,
        scrutinee_place: &CheckedPlace,
        scrutinee_type: &ResolvedType,
        domain: (i128, i128),
        lo: i128,
        hi: i128,
        span: Span,
        pointer_bits: u32,
    ) -> Vec<CheckedExprNode> {
        let mut conditions = Vec::new();
        if lo != domain.0 {
            let value = Self::i128_to_const_value(scrutinee_type, lo, pointer_bits);
            conditions.push(self.value_cmp_condition(
                scrutinee_place,
                span,
                scrutinee_type,
                BinaryOp::Ge,
                value,
            ));
        }
        if hi != domain.1 {
            let value = Self::i128_to_const_value(scrutinee_type, hi, pointer_bits);
            conditions.push(self.value_cmp_condition(
                scrutinee_place,
                span,
                scrutinee_type,
                BinaryOp::Le,
                value,
            ));
        }
        conditions
    }

    pub(super) fn i128_to_const_value(
        scrutinee_type: &ResolvedType,
        n: i128,
        pointer_bits: u32,
    ) -> ConstValue {
        match scrutinee_type {
            ResolvedType::Bool => ConstValue::Bool(n != 0),
            ResolvedType::Char => ConstValue::Char(
                char::from_u32(n as u32)
                    .expect("catch-all inference stays within char's own domain"),
            ),
            _ => match scrutinee_type.numeric_kind(pointer_bits) {
                Some(NumericKind::Signed(_)) => ConstValue::Number(NumberValue::Signed(n as i64)),
                Some(NumericKind::Unsigned(_)) => {
                    ConstValue::Number(NumberValue::Unsigned(n as u64))
                }
                _ => unreachable!(
                    "analyze_value_match only ever runs for an integer/bool/char scrutinee type"
                ),
            },
        }
    }

    fn const_eval_pattern(
        &mut self,
        expr: &HirExprNode,
        expected: &ResolvedType,
    ) -> Option<ConstValue> {
        match &expr.expr {
            HirExpr::Number(n) => self
                .const_number(expr.id, expr.span, n, expected, false)
                .map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => self
                    .const_number(expr.id, expr.span, n, expected, true)
                    .map(ConstValue::Number),
                _ => {
                    self.error(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::PatternValueNotConstant,
                    );
                    None
                }
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => {
                    self.error(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::PatternTypeMismatch {
                            expected: expected.clone(),
                            found: ResolvedType::Char,
                        },
                    );
                    None
                }
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => {
                    self.error(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::PatternTypeMismatch {
                            expected: expected.clone(),
                            found: ResolvedType::Bool,
                        },
                    );
                    None
                }
            },
            _ => {
                self.error(
                    expr.id,
                    expr.span,
                    AnalysisErrorKind::PatternValueNotConstant,
                );
                None
            }
        }
    }

    fn const_value_as_i128(value: &ConstValue) -> i128 {
        match value {
            ConstValue::Number(NumberValue::Signed(n)) => *n as i128,
            ConstValue::Number(NumberValue::Unsigned(n)) => *n as i128,
            ConstValue::Number(NumberValue::Float(_)) => {
                unreachable!(
                    "match patterns are never float-typed -- integer_domain excludes floats"
                )
            }
            ConstValue::Bool(b) => *b as i128,
            ConstValue::Char(c) => *c as i128,
            ConstValue::Str(_)
            | ConstValue::Slice(_)
            | ConstValue::Array(_)
            | ConstValue::Struct(_)
            | ConstValue::Enum { .. }
            | ConstValue::Union { .. }
            | ConstValue::Ref(_) => {
                unreachable!(
                    "analyze_value_match only ever runs for an integer/bool/char scrutinee type"
                )
            }
        }
    }

    fn value_cmp_condition(
        &mut self,
        scrutinee_place: &CheckedPlace,
        span: Span,
        scrutinee_type: &ResolvedType,
        op: BinaryOp,
        value: ConstValue,
    ) -> CheckedExprNode {
        let kind = match value {
            ConstValue::Number(n) => CheckedExpr::Number(n),
            ConstValue::Bool(b) => CheckedExpr::Bool(b),
            ConstValue::Char(c) => CheckedExpr::Char(c),
            ConstValue::Str(_)
            | ConstValue::Slice(_)
            | ConstValue::Array(_)
            | ConstValue::Struct(_)
            | ConstValue::Enum { .. }
            | ConstValue::Union { .. }
            | ConstValue::Ref(_) => {
                unreachable!(
                    "analyze_value_match only ever runs for an integer/bool/char scrutinee type"
                )
            }
        };
        let scrutinee_read = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: scrutinee_type.clone(),
            kind: CheckedExpr::Place(scrutinee_place.clone()),
        };
        let constant = CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: scrutinee_type.clone(),
            kind,
        };
        CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op,
                left: Box::new(scrutinee_read),
                right: Box::new(constant),
            }),
        }
    }

    fn describe_gap(scrutinee_type: &ResolvedType, lo: i128, hi: i128) -> String {
        let render = |n: i128| match scrutinee_type {
            ResolvedType::Bool => {
                if n == 0 {
                    "false".to_string()
                } else {
                    "true".to_string()
                }
            }
            ResolvedType::Char => match char::from_u32(n as u32) {
                Some(c) if c.is_ascii_graphic() || c == ' ' => format!("'{c}'"),
                _ => format!("U+{n:04X}"),
            },
            _ => n.to_string(),
        };
        if lo == hi {
            render(lo)
        } else {
            format!("{}..={}", render(lo), render(hi))
        }
    }

    fn unify_match_arm_types(
        &mut self,
        node_id: HirId,
        span: Span,
        arms: &[CheckedMatchArm],
        else_branch: &Option<CheckedBlock>,
    ) -> Option<ResolvedType> {
        let arm_kinds: Vec<Option<ResolvedType>> =
            arms.iter().map(|a| Self::block_type(&a.body)).collect();
        let else_kind: Option<Option<ResolvedType>> = else_branch.as_ref().map(Self::block_type);

        let result_type = arm_kinds
            .iter()
            .cloned()
            .chain(else_kind.iter().cloned())
            .flatten()
            .next()
            .map(|t| t.widened())
            .unwrap_or(ResolvedType::Void);

        let mismatch = arm_kinds
            .into_iter()
            .chain(else_kind)
            .flatten()
            .find(|t| !result_type.accepts(t));
        if let Some(found) = mismatch {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MatchArmTypeMismatch {
                    expected: result_type,
                    found,
                },
            );
            return None;
        }
        Some(result_type)
    }
}
