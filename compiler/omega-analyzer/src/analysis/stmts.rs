use super::*;

impl<'r> Analyzer<'r> {
    /// Whether an expression unconditionally diverges: only an `if`/`else
    /// if`/`else` can (with a genuine `else`, not an implicit empty one)
    /// where *every* branch diverges -- everything else either can't
    /// diverge at all, or (a bare `return`) isn't an expression to begin
    /// with. Needed because such an `if` still gets a concrete (if
    /// degenerate, `Void`) `r#type` of its own during analysis -- there's no
    /// real "never" `ResolvedType` to give it instead -- so whether *it*
    /// diverges has to be re-derived structurally here rather than read off
    /// its `r#type`, the same way `stmt_diverges` re-derives it for a bare
    /// `return` statement.
    pub(super) fn expr_diverges(expr: &CheckedExprNode) -> bool {
        match &expr.kind {
            CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                let Some(else_branch) = else_branch else { return false };
                branches.iter().all(|(_, b)| Self::block_type(b).is_none())
                    && Self::block_type(else_branch).is_none()
            }
            _ => false,
        }
    }

    /// Whether a statement unconditionally diverges (its block never
    /// actually reaches whatever position it's in): a plain `return`/
    /// `break`/`continue`, or an expression-statement that diverges (see
    /// `expr_diverges` -- currently only a fully-diverging `if`). This is
    /// still a purely syntactic check, not real reachability analysis (e.g.
    /// a `while true { return 1; }` with no way out isn't recognized as
    /// diverging), but "dispatch on a condition and return/break/continue
    /// from every arm" is common enough to be worth recognizing specifically
    /// (see `Codegen::emit_if`'s matching `BlockOutcome::Diverged`
    /// propagation, which this must stay in sync with -- codegen already
    /// builds sound IR for exactly this case).
    pub(super) fn stmt_diverges(stmt: &CheckedStmt) -> bool {
        match stmt {
            CheckedStmt::Return(_) | CheckedStmt::Break(_) | CheckedStmt::Continue(_) => true,
            CheckedStmt::Expression(expr) => Self::expr_diverges(expr),
            // `defer` never diverges at its own position -- it just marks
            // "reached" and moves on; the deferred body only ever runs later,
            // in the function's epilogue.
            CheckedStmt::Defer(_) => false,
            _ => false,
        }
    }

    /// Every `CheckedStmt` variant's id/span, for anchoring an
    /// `AnalysisWarningKind::UnreachableCode` at whichever statement turns
    /// out to be first made unreachable by a diverging predecessor (see
    /// `truncate_unreachable`).
    fn checked_stmt_id_span(stmt: &CheckedStmt) -> (HirId, Span) {
        match stmt {
            CheckedStmt::Declaration(d) => (d.id, d.span),
            CheckedStmt::ExternDeclaration(d) => (d.id, d.span),
            CheckedStmt::Expression(e) => (e.id, e.span),
            CheckedStmt::Return(e) => (e.id, e.span),
            CheckedStmt::While(w) => (w.id, w.span),
            CheckedStmt::For(f) => (f.id, f.span),
            CheckedStmt::Break(b) => (b.id, b.span),
            CheckedStmt::Continue(c) => (c.id, c.span),
            CheckedStmt::Defer(d) => (d.id, d.span),
        }
    }

    /// Drops every statement after the first one that unconditionally
    /// diverges (see `stmt_diverges`) -- they can never run, and keeping them
    /// in the `CheckedBlock` would make codegen try to emit instructions
    /// into an already-terminated cranelift block (a compiler panic, not a
    /// user-facing error; see `Codegen::emit_block`). Recorded as an
    /// `AnalysisWarningKind::UnreachableCode` rather than an `AnalysisError`:
    /// unlike everything else this pass rejects, dead code doesn't make the
    /// program incorrect, just wasteful -- the same reason real compilers
    /// warn about it instead of refusing to build.
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

    /// Walks a just-left scope's own declared bindings, warning about any
    /// that were never read (`UnusedVariable`/`UnusedParameter`, depending
    /// on `is_params`) or declared `mut` but never actually reassigned
    /// (`UnnecessaryMut`, gated on having been read at all -- see
    /// `VarBinding::written`'s doc comment for why a write-only binding
    /// reports as unused instead of unnecessarily-`mut`). Skips `narrowed`
    /// shadows (not user-declared) and, in a parameter scope, the implicit
    /// `self` (unused `self` is idiomatic in plenty of methods).
    pub(super) fn warn_unused_bindings(&mut self, scope: ScopeContext, is_params: bool) {
        // `declared_variables` is an `IndexMap` (see `ScopeContext`'s doc
        // comment) specifically so this walk visits bindings in declaration
        // order for free -- no sort needed here.
        for (name, binding) in &scope.declared_variables {
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
                self.warn(binding.decl_id, binding.span, AnalysisWarningKind::UnnecessaryMut { name: name.clone() });
            }
        }
    }

    /// Analyzes a `{ stmts... tail }` block in its own nested scope --
    /// shared by bare codeblock expressions, `if`/`while`/`for` bodies, and
    /// function bodies. Scope is always entered/left even on failure (before
    /// the `?` that can early-return), so an error partway through a block
    /// never leaves the scope stack unbalanced.
    /// `expected` is threaded *only* into the block's own tail expression
    /// (see `analyze_expr`'s doc comment) -- ordinary statements never have
    /// an outer expected type of their own.
    pub(super) fn analyze_block(&mut self, block: &HirBlock, expected: Option<&ResolvedType>) -> Option<CheckedBlock> {
        self.context.enter_scope();
        let checked_stmts = self.analyze_stmts(&block.stmts);
        let checked_tail = block.tail.as_ref().map(|e| self.analyze_expr(e, expected));
        let scope = self.context.leave_scope();
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

    /// `ident : Type = value;` -- builds the declaration and its
    /// initializing write by hand, exactly like `analyze_walrus` does,
    /// specifically so this write never goes through the ordinary
    /// `HirExpr::Assignment` arm's mutability check: it's the declaration's
    /// own initializer, never a `mut`-requiring reassignment, regardless of
    /// whether `ident` was declared `mut`.
    fn analyze_declaration_with_init(&mut self, decl: &HirDeclaration, value: &HirExprNode) -> Option<[CheckedStmt; 2]> {
        let checked_decl = self.analyze_declaration(decl, Storage::Local)?;
        let checked_value = self.analyze_expr(value, Some(&checked_decl.r#type))?;
        let checked_value = self.coerce_to_expected(Some(&checked_decl.r#type), checked_value);

        if !checked_decl.r#type.accepts(&checked_value.r#type) {
            self.error(
                value.id,
                value.span,
                AnalysisErrorKind::AssignmentTypeMismatch {
                    target: checked_decl.r#type.clone(),
                    value: checked_value.r#type.clone(),
                },
            );
            return None;
        }

        let declaration = CheckedStmt::Declaration(checked_decl.clone());
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: decl.id,
            span: decl.span,
            r#type: checked_decl.r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: decl.id,
                        storage: Storage::Local,
                        r#type: checked_decl.r#type,
                    },
                    projections: vec![],
                },
                value: Box::new(checked_value),
            }),
        });

        Some([declaration, assignment])
    }

    /// Desugars `ident := value;` into the same two `CheckedStmt`s writing
    /// `ident : <inferred type>; ident = value;` by hand would produce --
    /// analysis is the only place that can do this desugaring, since only it
    /// knows `value`'s resolved type (there's nothing written down to carry
    /// a type otherwise). `value` is analyzed exactly once and reused as the
    /// assignment's value, rather than re-analyzed, to avoid double-reporting
    /// any error inside it.
    fn analyze_walrus(&mut self, w: &HirWalrusDeclaration) -> Option<[CheckedStmt; 2]> {
        let checked_value = self.analyze_expr(&w.value, None)?;
        let r#type = checked_value.r#type.clone();
        self.declare_binding(w.id, w.span, &w.ident, r#type.clone(), Storage::Local, w.mutable)?;

        let declaration = CheckedStmt::Declaration(CheckedDeclaration {
            id: w.id,
            span: w.span,
            ident: w.ident.clone(),
            r#type: r#type.clone(),
        });
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: w.id,
            span: w.span,
            r#type: r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable { decl_id: w.id, storage: Storage::Local, r#type },
                    projections: vec![],
                },
                value: Box::new(checked_value),
            }),
        });

        Some([declaration, assignment])
    }

    /// Most statements analyze into exactly one `CheckedStmt`; a walrus
    /// declaration desugars into two (see `analyze_walrus`), which is why
    /// this returns a `Vec` rather than routing through the 1-to-1
    /// `analyze_all` fold.
    fn analyze_stmt(&mut self, stmt: &HirStmt) -> Option<Vec<CheckedStmt>> {
        match stmt {
            HirStmt::Declaration(decl) => {
                self.analyze_declaration(decl, Storage::Local).map(|d| vec![CheckedStmt::Declaration(d)])
            }
            HirStmt::DeclarationWithInit(decl, value) => {
                self.analyze_declaration_with_init(decl, value).map(Vec::from)
            }
            HirStmt::ExternDeclaration(decl) => {
                self.analyze_extern_decl(decl).map(|d| vec![CheckedStmt::ExternDeclaration(d)])
            }
            HirStmt::Expression(expr) => self.analyze_expr(expr, None).map(|e| {
                if matches!(e.kind, CheckedExpr::FunctionCall(_)) && e.r#type != ResolvedType::Void {
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
            HirStmt::WalrusDeclaration(w) => self.analyze_walrus(w).map(Vec::from),
            HirStmt::While(w) => {
                let checked_cond = self.analyze_expr(&w.condition, None)?;
                if checked_cond.r#type != ResolvedType::Bool {
                    self.error(
                        w.id,
                        checked_cond.span,
                        AnalysisErrorKind::NonBoolCondition { r#type: checked_cond.r#type },
                    );
                    return None;
                }
                self.loop_stack.push(w.id);
                let checked_body = self.analyze_block(&w.body, None);
                self.loop_stack.pop();
                let checked_body = checked_body?;
                Some(vec![CheckedStmt::While(CheckedWhile {
                    id: w.id,
                    span: w.span,
                    condition: checked_cond,
                    body: checked_body,
                })])
            }
            HirStmt::For(f) => self.analyze_for(f),
            HirStmt::Break(b) => match self.loop_stack.last() {
                Some(&loop_id) => Some(vec![CheckedStmt::Break(CheckedBreak { id: b.id, span: b.span, loop_id })]),
                None => {
                    self.error(b.id, b.span, AnalysisErrorKind::BreakOutsideLoop);
                    None
                }
            },
            HirStmt::Continue(c) => match self.loop_stack.last() {
                Some(&loop_id) => {
                    Some(vec![CheckedStmt::Continue(CheckedContinue { id: c.id, span: c.span, loop_id })])
                }
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
                let previous_in_defer_body = std::mem::replace(&mut self.in_defer_body, true);
                let body = self.analyze_block(&d.body, None);
                self.in_defer_body = previous_in_defer_body;
                let body = body?;
                Some(vec![CheckedStmt::Defer(CheckedDefer { id: d.id, span: d.span, body })])
            }
        }
    }

    /// `for`'s init/condition/post/body all share one scope of their own
    /// (so an `i := 0` init clause is visible to the condition/post/body
    /// but doesn't leak past the loop) -- entered once here, around all
    /// four, rather than the body getting its own additional nested scope
    /// from `analyze_block` alone. Every sub-part is still analyzed even
    /// after an earlier one fails (same "keep going, report everything"
    /// discipline as `analyze_all`), and the scope is always left before
    /// any early return.
    fn analyze_for(&mut self, f: &HirFor) -> Option<Vec<CheckedStmt>> {
        self.context.enter_scope();

        let mut ok = true;

        let checked_init = self.analyze_stmts(&f.init);
        ok &= checked_init.is_some();

        let checked_condition = match &f.condition {
            Some(c) => match self.analyze_expr(c, None) {
                Some(cc) if cc.r#type != ResolvedType::Bool => {
                    self.error(f.id, cc.span, AnalysisErrorKind::NonBoolCondition { r#type: cc.r#type });
                    ok = false;
                    None
                }
                Some(cc) => Some(cc),
                None => {
                    ok = false;
                    None
                }
            },
            None => {
                self.error(f.id, f.span, AnalysisErrorKind::ForLoopMissingCondition);
                ok = false;
                None
            }
        };

        let checked_post = match &f.post {
            Some(p) => {
                let checked = self.analyze_expr(p, None);
                ok &= checked.is_some();
                checked
            }
            None => None,
        };

        self.loop_stack.push(f.id);
        let checked_body = self.analyze_block(&f.body, None);
        self.loop_stack.pop();
        ok &= checked_body.is_some();

        let scope = self.context.leave_scope();
        self.warn_unused_bindings(scope, false);

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
