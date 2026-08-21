use super::*;

enum ForInSource {
    ToIterator(ResolvedBound),
    DirectIterator(CheckedExprNode),
}

impl<'r> Analyzer<'r> {
    pub(super) fn expr_diverges(expr: &CheckedExprNode) -> bool {
        if expr.r#type == ResolvedType::Never {
            return true;
        }
        match &expr.kind {
            CheckedExpr::If(CheckedIf {
                branches,
                else_branch,
            }) => {
                let Some(else_branch) = else_branch else {
                    return false;
                };
                branches.iter().all(|(_, b)| Self::block_type(b).is_none())
                    && Self::block_type(else_branch).is_none()
            }
            _ => false,
        }
    }

    pub(super) fn stmt_diverges(stmt: &CheckedStmt) -> bool {
        match stmt {
            CheckedStmt::Return(_) | CheckedStmt::Break(_) | CheckedStmt::Continue(_) => true,
            CheckedStmt::Expression(expr) => Self::expr_diverges(expr),
            CheckedStmt::Loop(l) => !l.has_break,
            CheckedStmt::Defer(_) => false,
            _ => false,
        }
    }

    fn checked_stmt_id_span(stmt: &CheckedStmt) -> (HirId, Span) {
        match stmt {
            CheckedStmt::Declaration(d) => (d.id, d.span),
            CheckedStmt::ExternDeclaration(d) => (d.id, d.span),
            CheckedStmt::Expression(e) => (e.id, e.span),
            CheckedStmt::Return(e) => (e.id, e.span),
            CheckedStmt::While(w) => (w.id, w.span),
            CheckedStmt::Loop(l) => (l.id, l.span),
            CheckedStmt::For(f) => (f.id, f.span),
            CheckedStmt::Break(b) => (b.id, b.span),
            CheckedStmt::Continue(c) => (c.id, c.span),
            CheckedStmt::Defer(d) => (d.id, d.span),
            CheckedStmt::InlineAsm(a) => (a.id, a.span),
        }
    }

    fn truncate_unreachable(&mut self, mut stmts: Vec<CheckedStmt>) -> Vec<CheckedStmt> {
        let Some(cutoff) = stmts.iter().position(Self::stmt_diverges) else {
            return stmts;
        };
        if let Some(first_dead) = stmts.get(cutoff + 1) {
            let (id, span) = Self::checked_stmt_id_span(first_dead);
            self.warn(id, span, AnalysisWarningKind::UnreachableCode);
        }
        stmts.truncate(cutoff + 1);
        stmts
    }

    pub(super) fn warn_unused_bindings(&mut self, scope: LexicalScope, is_params: bool) {
        for ((name, origin), binding) in scope.bindings() {
            if origin.0.is_some() {
                continue;
            }
            if binding.narrowed || (is_params && name.as_ref() == "self") {
                continue;
            }
            if !binding.used {
                let kind = if is_params {
                    AnalysisWarningKind::UnusedParameter { name: name.clone() }
                } else {
                    AnalysisWarningKind::UnusedVariable { name: name.clone() }
                };
                self.warn(binding.decl_id, binding.span, kind);
            } else if binding.mutable && !binding.written {
                self.warn(
                    binding.decl_id,
                    binding.span,
                    AnalysisWarningKind::UnnecessaryMut { name: name.clone() },
                );
            }
        }
    }

    pub(super) fn analyze_block(
        &mut self,
        block: &HirBlock,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedBlock> {
        let ((checked_stmts, checked_tail), scope) = self.with_scope(|this| {
            let stmts = this.analyze_stmts(&block.stmts);
            let tail = block.tail.as_ref().map(|expr| {
                this.analyze_expr(expr, expected)
                    .map(|value| this.coerce_to_expected(expected, value))
            });
            (stmts, tail)
        });
        self.warn_unused_bindings(scope, false);

        let stmts = checked_stmts?;
        let tail = match checked_tail {
            Some(t) => Some(Box::new(t?)),
            None => None,
        };

        // `analyze_stmts` already truncated (and warned about) unreachable
        // statements *within* `stmts`; if what's left still ends in
        // something that diverges, a tail expression after it -- if any --
        // is unreachable too, for the same reason.
        let tail = match &tail {
            Some(t) if stmts.last().is_some_and(Self::stmt_diverges) => {
                self.warn(t.id, t.span, AnalysisWarningKind::UnreachableCode);
                None
            }
            _ => tail,
        };

        Some(CheckedBlock { stmts, tail })
    }

    fn analyze_declaration_with_init(
        &mut self,
        decl: &HirDeclaration,
        value: &HirExprNode,
    ) -> Option<[CheckedStmt; 2]> {
        let (resolved_type, checked_value) =
            self.resolve_typed_decl_init(decl.id, decl.span, &decl.r#type, value)?;
        self.declare_binding(
            decl.id,
            decl.span,
            &decl.ident,
            decl.origin,
            resolved_type.clone(),
            Storage::Local,
            decl.mutable,
        )?;
        let checked_decl = CheckedDeclaration {
            id: decl.id,
            span: decl.span,
            ident: decl.ident.clone(),
            r#type: resolved_type.clone(),
            mutable: decl.mutable,
            initial_value: None,
        };

        let declaration = CheckedStmt::Declaration(checked_decl);
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span: decl.span,
            r#type: resolved_type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: decl.id,
                        storage: Storage::Local,
                        r#type: resolved_type.clone(),
                    },
                    projections: vec![],
                    r#type: resolved_type,
                },
                value: Box::new(checked_value),
            }),
        });

        Some([declaration, assignment])
    }

    fn analyze_walrus(&mut self, w: &HirWalrusDeclaration) -> Option<Vec<CheckedStmt>> {
        let checked_value = self.analyze_expr(&w.value, None)?;
        let r#type = checked_value.r#type.clone();

        if w.comp {
            if w.mutable {
                self.error(w.id, w.span, AnalysisErrorKind::MutCompBinding);
                return None;
            }
            let value = self.eval_comp(w.id, &checked_value)?;
            self.declare_comp_binding(w.id, w.span, &w.ident, w.origin, r#type, value)?;
            return Some(vec![]);
        }

        self.declare_binding(
            w.id,
            w.span,
            &w.ident,
            w.origin,
            r#type.clone(),
            Storage::Local,
            w.mutable,
        )?;

        let declaration = CheckedStmt::Declaration(CheckedDeclaration {
            id: w.id,
            span: w.span,
            ident: w.ident.clone(),
            r#type: r#type.clone(),
            mutable: w.mutable,
            initial_value: None,
        });
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span: w.span,
            r#type: r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: w.id,
                        storage: Storage::Local,
                        r#type: r#type.clone(),
                    },
                    projections: vec![],
                    r#type,
                },
                value: Box::new(checked_value),
            }),
        });

        Some(vec![declaration, assignment])
    }

    fn analyze_stmt(&mut self, stmt: &HirStmt) -> Option<Vec<CheckedStmt>> {
        match stmt {
            HirStmt::Declaration(decl) => self
                .analyze_declaration(decl, Storage::Local)
                .map(|d| vec![CheckedStmt::Declaration(d)]),
            HirStmt::DeclarationWithInit(decl, value) => self
                .analyze_declaration_with_init(decl, value)
                .map(Vec::from),
            HirStmt::ExternDeclaration(decl) => self
                .analyze_extern_decl(decl)
                .map(|d| vec![CheckedStmt::ExternDeclaration(d)]),
            HirStmt::Expression(expr) => self.analyze_expr(expr, None).map(|e| {
                if matches!(e.kind, CheckedExpr::FunctionCall(_))
                    && e.r#type != ResolvedType::Void
                    && e.r#type != ResolvedType::Never
                {
                    self.warn(e.id, e.span, AnalysisWarningKind::UnusedReturnValue);
                }
                vec![CheckedStmt::Expression(e)]
            }),
            HirStmt::Return(expr) => {
                if self.in_defer_body {
                    self.error(expr.id, expr.span, AnalysisErrorKind::ReturnInsideDefer);
                    return None;
                }
                let return_type = self.current_return_type.clone();
                let checked = self.analyze_expr(expr, Some(&return_type))?;
                let checked = self.coerce_to_expected(Some(&return_type), checked);
                if !self.current_return_type.accepts(&checked.r#type) {
                    self.error(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::ReturnTypeMismatch {
                            expected: self.current_return_type.clone(),
                            found: checked.r#type.clone(),
                        },
                    );
                    return None;
                }
                Some(vec![CheckedStmt::Return(checked)])
            }
            HirStmt::WalrusDeclaration(w) => self.analyze_walrus(w),
            HirStmt::While(w) => {
                let checked_cond = self.analyze_expr(&w.condition, None)?;
                if checked_cond.r#type != ResolvedType::Bool {
                    self.error(
                        w.id,
                        checked_cond.span,
                        AnalysisErrorKind::NonBoolCondition {
                            r#type: checked_cond.r#type,
                        },
                    );
                    return None;
                }
                // Best-effort: unlike `eval_comp` (for an explicit `comp`
                // expression), a non-constant condition here just isn't a
                // match for the warning, not a compile error, so any `Err`
                // is silently ignored.
                if let Ok(ConstValue::Bool(true)) =
                    crate::comp_eval::eval(self.resolver, &checked_cond, self.target)
                {
                    self.warn(w.id, checked_cond.span, AnalysisWarningKind::PreferLoop);
                }
                let checked_body =
                    self.with_loop(w.id, |this| this.analyze_block(&w.body, None))?;
                Some(vec![CheckedStmt::While(CheckedWhile {
                    id: w.id,
                    span: w.span,
                    condition: checked_cond,
                    body: checked_body,
                })])
            }
            HirStmt::Loop(l) => {
                let checked_body =
                    self.with_loop(l.id, |this| this.analyze_block(&l.body, None))?;
                Some(vec![CheckedStmt::Loop(CheckedLoop {
                    id: l.id,
                    span: l.span,
                    has_break: self.loops_with_break.contains(&l.id),
                    body: checked_body,
                })])
            }
            HirStmt::For(f) => self.analyze_for(f),
            HirStmt::ForIn(f) => self.analyze_for_in(f),
            HirStmt::Break(b) => match self.loop_stack.last() {
                Some(&loop_id) => {
                    self.loops_with_break.insert(loop_id);
                    Some(vec![CheckedStmt::Break(CheckedBreak {
                        id: b.id,
                        span: b.span,
                        loop_id,
                    })])
                }
                None => {
                    self.error(b.id, b.span, AnalysisErrorKind::BreakOutsideLoop);
                    None
                }
            },
            HirStmt::Continue(c) => match self.loop_stack.last() {
                Some(&loop_id) => Some(vec![CheckedStmt::Continue(CheckedContinue {
                    id: c.id,
                    span: c.span,
                    loop_id,
                })]),
                None => {
                    self.error(c.id, c.span, AnalysisErrorKind::ContinueOutsideLoop);
                    None
                }
            },
            HirStmt::Defer(d) => {
                if !self.loop_stack.is_empty() {
                    self.error(d.id, d.span, AnalysisErrorKind::DeferInsideLoopNotSupported);
                    return None;
                }
                if self.in_defer_body {
                    self.error(d.id, d.span, AnalysisErrorKind::NestedDeferNotSupported);
                    return None;
                }
                let body = self.with_defer_body(|this| this.analyze_block(&d.body, None))?;
                Some(vec![CheckedStmt::Defer(CheckedDefer {
                    id: d.id,
                    span: d.span,
                    body,
                })])
            }
            HirStmt::InlineAsm(asm) => self.analyze_inline_asm(asm),
        }
    }

    fn analyze_for(&mut self, f: &HirFor) -> Option<Vec<CheckedStmt>> {
        let (result, scope) = self.with_scope(|this| {
            let mut ok = true;

            let checked_init = this.analyze_stmts(&f.init);
            ok &= checked_init.is_some();

            let checked_condition = match &f.condition {
                Some(condition) => match this.analyze_expr(condition, None) {
                    Some(checked) if checked.r#type != ResolvedType::Bool => {
                        this.error(
                            f.id,
                            checked.span,
                            AnalysisErrorKind::NonBoolCondition {
                                r#type: checked.r#type,
                            },
                        );
                        ok = false;
                        None
                    }
                    Some(checked) => Some(checked),
                    None => {
                        ok = false;
                        None
                    }
                },
                None => {
                    this.error(f.id, f.span, AnalysisErrorKind::ForLoopMissingCondition);
                    ok = false;
                    None
                }
            };

            let checked_post = match &f.post {
                Some(post) => {
                    let checked = this.analyze_expr(post, None);
                    ok &= checked.is_some();
                    checked
                }
                None => None,
            };

            let checked_body = this.with_loop(f.id, |this| this.analyze_block(&f.body, None));
            ok &= checked_body.is_some();

            if !ok {
                return None;
            }

            Some(vec![CheckedStmt::For(Box::new(CheckedFor {
                id: f.id,
                span: f.span,
                init: checked_init?,
                condition: checked_condition?,
                post: checked_post,
                body: checked_body?,
            }))])
        });
        self.warn_unused_bindings(scope, false);
        result
    }

    fn analyze_for_in(&mut self, f: &HirForIn) -> Option<Vec<CheckedStmt>> {
        let (result, scope) = self.with_scope(|this| {
            let iter_init = match this.classify_for_in_source(f) {
                Some(ForInSource::ToIterator(selected)) => {
                    this.with_bounds(std::iter::once(selected), |this| {
                        this.synthesize_method_call(
                            HirPlaceRoot::Expr(Box::new(f.iterator.clone())),
                            "to_iterator",
                            f.span,
                        )
                    })
                }
                Some(ForInSource::DirectIterator(checked)) => Some(checked),
                None => None,
            }?;

            let iter_id = this.resolver.fresh_synthetic_id();
            let iter_type = iter_init.r#type.clone();
            this.declare_binding(
                iter_id,
                f.span,
                &Ident("$iter".to_string()),
                Origin::default(),
                iter_type.clone(),
                Storage::Local,
                true,
            );
            let (iter_decl, iter_assign) =
                this.synthetic_declaration(iter_id, f.span, "$iter", iter_type.clone(), iter_init);

            let bounds = this
                .resolver
                .conformances_for_type(&iter_type)
                .unwrap_or_default()
                .into_iter()
                .map(|conform| ResolvedBound::new(conform.target, conform.spec, conform.spec_args));
            let while_stmt = this.with_bounds(bounds, |this| this.analyze_for_in_loop(f))?;
            Some(vec![iter_decl, iter_assign, while_stmt])
        });
        self.warn_unused_bindings(scope, false);
        result
    }

    fn classify_for_in_source(&mut self, f: &HirForIn) -> Option<ForInSource> {
        let errors_before = self.errors.len();
        let warnings_before = self.warnings.len();
        let Some(checked) = self.analyze_expr(&f.iterator, None) else {
            return None;
        };

        let to_iterator = self.for_in_conformances(&checked.r#type, "ToIterator");
        if !to_iterator.is_empty() {
            let expected_element = f
                .binding_type
                .as_ref()
                .and_then(|raw| self.resolve_type_or_error(f.id, f.span, raw, true));
            let available: Vec<ResolvedType> = to_iterator
                .iter()
                .filter_map(|conform| conform.spec_args.first().cloned())
                .collect();
            let candidates: Vec<_> = to_iterator
                .into_iter()
                .filter(|conform| {
                    expected_element
                        .as_ref()
                        .is_none_or(|expected| conform.spec_args.first() == Some(expected))
                })
                .collect();
            if candidates.len() == 1 {
                let conform = candidates.into_iter().next().expect("length checked");
                // `to_iterator` is synthesized from the original HIR and will
                // analyze the source expression again. Keep semantic side
                // effects from this classification pass, but avoid emitting
                // its diagnostics twice.
                self.errors.truncate(errors_before);
                self.warnings.truncate(warnings_before);
                return Some(ForInSource::ToIterator(ResolvedBound {
                    target: conform.target,
                    spec: conform.spec,
                    spec_args: conform.spec_args,
                }));
            }
            let kind = match expected_element {
                Some(expected) if candidates.is_empty() => {
                    AnalysisErrorKind::ForLoopElementTypeMismatch {
                        expected,
                        available,
                    }
                }
                _ => AnalysisErrorKind::AmbiguousForLoopElementType {
                    candidates: available,
                },
            };
            self.error(f.id, f.span, kind);
            return None;
        }
        if self.for_in_source_declares(&checked.r#type, "Iterator") {
            return Some(ForInSource::DirectIterator(checked));
        }
        self.error(
            f.id,
            f.span,
            AnalysisErrorKind::ForLoopSourceNotIterable {
                r#type: checked.r#type,
            },
        );
        None
    }

    fn analyze_for_in_loop(&mut self, f: &HirForIn) -> Option<CheckedStmt> {
        let while_id = self.resolver.fresh_synthetic_id();
        let body = self.with_loop(while_id, |this| {
            this.with_scope(|this| {
                let iter_read = HirPlaceRoot::Path(ExprPath::from(Ident("$iter".to_string())));
                let next = this.synthesize_method_call(iter_read, "next", f.span)?;

                let next_id = this.resolver.fresh_synthetic_id();
                let next_type = next.r#type.clone();
                let ResolvedType::Enum {
                    cell: option_cell, ..
                } = next_type.clone()
                else {
                    unreachable!(
                        "Iterator::next's declared return type is always an Option<T> enum"
                    );
                };
                this.declare_binding(
                    next_id,
                    f.span,
                    &Ident("$next".to_string()),
                    Origin::default(),
                    next_type.clone(),
                    Storage::Local,
                    false,
                );
                let (next_decl, next_assign) =
                    this.synthetic_declaration(next_id, f.span, "$next", next_type.clone(), next);

                let mut tag_projections = Vec::new();
                let tag_type = this.resolve_field_projection(
                    f.id,
                    f.span,
                    &mut tag_projections,
                    &next_type,
                    &Ident("tag".to_string()),
                    &mut false,
                )?;
                let tag_place = CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: next_id,
                        storage: Storage::Local,
                        r#type: next_type,
                    },
                    projections: tag_projections,
                    r#type: tag_type.clone(),
                };

                let none_arm = this.for_in_none_arm(
                    f,
                    while_id,
                    next_id,
                    &option_cell,
                    &tag_type,
                    &tag_place,
                )?;
                let some_arm =
                    this.for_in_some_arm(f, next_id, &option_cell, &tag_type, &tag_place)?;

                let match_expr = CheckedStmt::Expression(CheckedExprNode {
                    id: this.resolver.fresh_synthetic_id(),
                    span: f.span,
                    r#type: ResolvedType::Void,
                    kind: CheckedExpr::Match(CheckedMatch {
                        arms: vec![none_arm, some_arm],
                        else_branch: None,
                    }),
                });

                Some(CheckedBlock {
                    stmts: vec![next_decl, next_assign, match_expr],
                    tail: None,
                })
            })
            .0
        })?;

        Some(CheckedStmt::While(CheckedWhile {
            id: while_id,
            span: f.span,
            condition: CheckedExprNode {
                id: self.resolver.fresh_synthetic_id(),
                span: f.span,
                r#type: ResolvedType::Bool,
                kind: CheckedExpr::Bool(true),
            },
            body,
        }))
    }

    fn for_in_none_arm(
        &mut self,
        f: &HirForIn,
        while_id: HirId,
        next_id: HirId,
        option_cell: &Rc<RefCell<ResolvedEnumType>>,
        tag_type: &ResolvedType,
        tag_place: &CheckedPlace,
    ) -> Option<CheckedMatchArm> {
        let (body, scope) = self.with_scope(|this| {
            let refined = ResolvedType::Enum {
                cell: option_cell.clone(),
                variant: Some(0),
            };
            this.declare_narrowed_binding(
                next_id,
                f.span,
                &Ident("$next".to_string()),
                Origin::default(),
                refined,
                Storage::Local,
                false,
            );
            CheckedBlock {
                stmts: vec![CheckedStmt::Break(CheckedBreak {
                    id: this.resolver.fresh_synthetic_id(),
                    span: f.span,
                    loop_id: while_id,
                })],
                tail: None,
            }
        });
        self.warn_unused_bindings(scope, false);

        let condition = self.tag_equals(
            f.span,
            tag_type,
            tag_place,
            option_cell.borrow().variants[0].tag,
        );
        Some(CheckedMatchArm {
            conditions: vec![vec![condition]],
            body,
        })
    }

    fn for_in_some_arm(
        &mut self,
        f: &HirForIn,
        next_id: HirId,
        option_cell: &Rc<RefCell<ResolvedEnumType>>,
        tag_type: &ResolvedType,
        tag_place: &CheckedPlace,
    ) -> Option<CheckedMatchArm> {
        let refined = ResolvedType::Enum {
            cell: option_cell.clone(),
            variant: Some(1),
        };
        let (body, scope) = self.with_scope(|this| {
            this.declare_narrowed_binding(
                next_id,
                f.span,
                &Ident("$next".to_string()),
                Origin::default(),
                refined.clone(),
                Storage::Local,
                false,
            );

            let mut value_projections = Vec::new();
            let value_type = this.resolve_field_projection(
                f.id,
                f.span,
                &mut value_projections,
                &refined,
                &Ident("value".to_string()),
                &mut false,
            )?;
            let value_read = CheckedExprNode {
                id: this.resolver.fresh_synthetic_id(),
                span: f.span,
                r#type: value_type.clone(),
                kind: CheckedExpr::Place(CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: next_id,
                        storage: Storage::Local,
                        r#type: refined,
                    },
                    projections: value_projections,
                    r#type: value_type.clone(),
                }),
            };

            let binding_id = this.resolver.fresh_synthetic_id();
            this.declare_binding(
                binding_id,
                f.span,
                &f.binding,
                Origin::default(),
                value_type.clone(),
                Storage::Local,
                f.mutable,
            );
            let (binding_decl, binding_assign) = this.synthetic_declaration(
                binding_id,
                f.span,
                f.binding.as_ref(),
                value_type,
                value_read,
            );

            let user_stmts = this.analyze_stmts(&f.body.stmts)?;
            let user_tail = match &f.body.tail {
                Some(tail) => Some(Box::new(this.analyze_expr(tail, None)?)),
                None => None,
            };

            let mut stmts = vec![binding_decl, binding_assign];
            stmts.extend(user_stmts);
            Some(CheckedBlock {
                stmts,
                tail: user_tail,
            })
        });
        self.warn_unused_bindings(scope, false);

        let condition = self.tag_equals(
            f.span,
            tag_type,
            tag_place,
            option_cell.borrow().variants[1].tag,
        );
        Some(CheckedMatchArm {
            conditions: vec![vec![condition]],
            body: body?,
        })
    }

    fn tag_equals(
        &mut self,
        span: Span,
        tag_type: &ResolvedType,
        tag_place: &CheckedPlace,
        tag: NumberValue,
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
            kind: CheckedExpr::Number(tag),
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

    fn synthesize_method_call(
        &mut self,
        root: HirPlaceRoot,
        method: &str,
        span: Span,
    ) -> Option<CheckedExprNode> {
        let callee = HirExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            expr: HirExpr::Place(HirPlace {
                root,
                projections: vec![HirProjection::FieldAccess(Ident(method.to_string()))],
            }),
        };
        let call = HirExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            expr: HirExpr::FunctionCall(HirFunctionCall {
                callee: Box::new(callee),
                args: vec![],
            }),
        };
        self.analyze_expr(&call, None)
    }

    fn synthetic_declaration(
        &mut self,
        id: HirId,
        span: Span,
        name: &str,
        r#type: ResolvedType,
        value: CheckedExprNode,
    ) -> (CheckedStmt, CheckedStmt) {
        let ident = Ident(name.to_string());
        let declaration = CheckedStmt::Declaration(CheckedDeclaration {
            id,
            span,
            ident,
            r#type: r#type.clone(),
            mutable: true,
            initial_value: None,
        });
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: self.resolver.fresh_synthetic_id(),
            span,
            r#type: r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: id,
                        storage: Storage::Local,
                        r#type: r#type.clone(),
                    },
                    projections: vec![],
                    r#type,
                },
                value: Box::new(value),
            }),
        });
        (declaration, assignment)
    }

    fn analyze_stmts(&mut self, stmts: &[HirStmt]) -> Option<Vec<CheckedStmt>> {
        let mut checked = Vec::with_capacity(stmts.len());
        let mut ok = true;
        for stmt in stmts {
            match self.analyze_stmt(stmt) {
                Some(mut items) => checked.append(&mut items),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        Some(self.truncate_unreachable(checked))
    }
}
