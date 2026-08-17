use super::*;

/// What `Analyzer::classify_for_in_source` found `f.iterator`'s type
/// nominally declares -- `ToIterator<T>` (the common case, needing an
/// actual `.to_iterator()` call to produce `$iter`) or `Iterator<T>`
/// directly (the source *is* already an iterator; `f.iterator`'s own
/// already-checked value becomes `$iter` verbatim, no method call at all).
enum ForInSource {
    ToIterator(CheckedExprNode, ResolvedBound),
    DirectIterator(CheckedExprNode),
}

impl<'r> Analyzer<'r> {
    /// Whether an expression unconditionally diverges. Two independent
    /// cases:
    ///
    /// - A call whose resolved type is `ResolvedType::Never` -- ordinary
    ///   type inference for a function call already sets a call
    ///   expression's own `r#type` to its callee's return type verbatim,
    ///   so a call to a `never`-declared function already carries `Never`
    ///   right there with no extra plumbing; this just has to recognize
    ///   what that means.
    /// - An `if`/`else if`/`else` (with a genuine `else`, not an implicit
    ///   empty one) where *every* branch diverges -- re-derived
    ///   structurally rather than read off `expr.r#type`, because
    ///   `analyze_if` still gives such an `if` a concrete (degenerate
    ///   `Void`) type of its own rather than `Never` (nothing needs it to
    ///   be `Never`, since this function already re-derives the fact
    ///   directly).
    ///
    /// Everything else either can't diverge at all, or (a bare `return`)
    /// isn't an expression to begin with.
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
            // A `loop { }` with no `break` anywhere targeting it (recorded
            // once, at analysis time -- see `CheckedLoop::has_break`'s doc
            // comment) always repeats. Purely syntactic, same spirit as
            // every other case here: `loop { if cond { break; } }` is *not*
            // recognized as diverging, even though it happens to loop
            // forever whenever `cond` is false -- conservative and sound,
            // not a weakness specific to this case.
            CheckedStmt::Loop(l) => !l.has_break,
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
            CheckedStmt::Loop(l) => (l.id, l.span),
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
        for ((name, origin), binding) in &scope.declared_variables {
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

    /// Analyzes a `{ stmts... tail }` block in its own nested scope --
    /// shared by bare codeblock expressions, `if`/`while`/`for` bodies, and
    /// function bodies. Scope is always entered/left even on failure (before
    /// the `?` that can early-return), so an error partway through a block
    /// never leaves the scope stack unbalanced.
    /// `expected` is threaded *only* into the block's own tail expression
    /// (see `analyze_expr`'s doc comment) -- ordinary statements never have
    /// an outer expected type of their own.
    pub(super) fn analyze_block(
        &mut self,
        block: &HirBlock,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedBlock> {
        self.context.enter_scope();
        let checked_stmts = self.analyze_stmts(&block.stmts);
        let checked_tail = block.tail.as_ref().map(|e| {
            self.analyze_expr(e, expected)
                .map(|value| self.coerce_to_expected(expected, value))
        });
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
            id: decl.id,
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

    /// Desugars `ident := value;` into the same two `CheckedStmt`s writing
    /// `ident : <inferred type>; ident = value;` by hand would produce --
    /// analysis is the only place that can do this desugaring, since only it
    /// knows `value`'s resolved type (there's nothing written down to carry
    /// a type otherwise). `value` is analyzed exactly once and reused as the
    /// assignment's value, rather than re-analyzed, to avoid double-reporting
    /// any error inside it.
    ///
    /// `comp ident := value;` desugars into *nothing* instead (an empty
    /// `Vec`): a `comp` binding carries no storage at all, so there is no
    /// declaration and no assignment to emit -- `value` is evaluated once,
    /// right here, and every later reference substitutes the result
    /// directly (see `Analyzer::declare_comp_binding`/`analyze_place_read`).
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
            // A local's value comes from the `Assignment` statement right
            // below, never baked into the declaration itself -- unlike a
            // global, whose `initial_value` this pattern's own doc comment
            // covers.
            initial_value: None,
        });
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: w.id,
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

    /// Most statements analyze into exactly one `CheckedStmt`; a walrus
    /// declaration desugars into two (see `analyze_walrus`), which is why
    /// this returns a `Vec` rather than routing through the 1-to-1
    /// `analyze_all` fold.
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
                // A `never`-typed call has no return value to discard in
                // the first place -- it never returns at all, so there's
                // nothing "unused" about leaving it as a bare statement.
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
                // Best-effort: a non-constant condition (references a
                // variable, calls a function, ...) just isn't a match for
                // this warning, not a compile error -- unlike
                // `Analyzer::eval_comp`, which is for an explicit `comp`
                // expression the author committed to being constant, this
                // is purely opportunistic, so any `Err` is silently
                // ignored rather than reported.
                if let Ok(ConstValue::Bool(true)) =
                    crate::comp_eval::eval(self.resolver, &checked_cond, self.target)
                {
                    self.warn(w.id, checked_cond.span, AnalysisWarningKind::PreferLoop);
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
            HirStmt::Loop(l) => {
                self.loop_stack.push(l.id);
                let checked_body = self.analyze_block(&l.body, None);
                self.loop_stack.pop();
                let checked_body = checked_body?;
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
                let previous_in_defer_body = std::mem::replace(&mut self.in_defer_body, true);
                let body = self.analyze_block(&d.body, None);
                self.in_defer_body = previous_in_defer_body;
                let body = body?;
                Some(vec![CheckedStmt::Defer(CheckedDefer {
                    id: d.id,
                    span: d.span,
                    body,
                })])
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
                    self.error(
                        f.id,
                        cc.span,
                        AnalysisErrorKind::NonBoolCondition { r#type: cc.r#type },
                    );
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

    /// `for <mut>? binding in iterator { body }` -- desugars entirely at
    /// analysis time into the `while true { }`+`match` shape a hand-written
    /// equivalent would use, reusing already-proven machinery rather than
    /// adding any new MIR/codegen surface:
    ///
    /// ```text
    /// {
    ///     $iter := <iterator>.to_iterator();
    ///     while true {
    ///         $next := $iter.next();
    ///         match $next {
    ///             Option::None => { break; }
    ///             Option::Some => {
    ///                 <mut>? binding := $next.value;
    ///                 <body, spliced in unchanged>
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// The two method calls (`to_iterator`/`next`) are resolved by
    /// synthesizing a small amount of ordinary, source-shaped HIR (see
    /// `synthesize_method_call`) and feeding it through `analyze_expr` --
    /// the same auto-ref/overload-resolution/static-vs-dynamic-dispatch
    /// selection any hand-written `x.method()` call already goes through,
    /// not reimplemented here. The `match`, by contrast, is hand-built
    /// directly (`resolve_field_projection`/`declare_narrowed_binding`,
    /// the same primitives `analyze_enum_match` itself uses) rather than
    /// synthesized as HIR and re-parsed, because this language's `match`
    /// has no destructuring pattern syntax at all -- `Option::Some`
    /// doesn't bind a name on its own; only *narrowing* an already-named
    /// scrutinee does (see `analyze_enum_match`'s own doc comment), and
    /// `$next` is a synthetic local with no source-level name a pattern
    /// could reference in the first place.
    ///
    /// `core::option::Option`'s variant order is load-bearing here --
    /// `None` is hardcoded as variant 0, `Some` as variant 1 (see
    /// `runtime/core/option.omg`'s own doc comment).
    ///
    /// Real, nominal conformance -- **not** duck-typed -- is checked first,
    /// via `classify_for_in_source`: a type that merely happens to have a
    /// same-shaped `to_iterator`/`next` method, without a matching conform
    /// declaration for `ToIterator<T>`/`Iterator<T>`, is rejected with
    /// `ForLoopSourceNotIterable` instead of silently accepted the way this
    /// desugaring originally worked (`synthesize_method_call` resolves a
    /// method purely by name/shape, with no notion of a declared spec at
    /// all -- true of `to_iterator` below just as much as of `next` in
    /// `analyze_for_in_loop`, but only `to_iterator`'s receiver is a type
    /// this feature doesn't otherwise already know implements the right
    /// spec; `next`'s receiver, `$iter`, is either `to_iterator`'s own
    /// return type or `f.iterator` itself, both already proven to implement
    /// `Iterator<T>` by construction).
    ///
    /// The source may declare **either** `ToIterator<T>` (the common case
    /// -- a collection producing a fresh iterator) **or** `Iterator<T>`
    /// directly (the source *is* already an iterator/cursor) -- mirroring
    /// Rust's blanket `impl<I: Iterator> IntoIterator for I`. `ToIterator`
    /// is tried first when a type declares both (matching Rust: an
    /// explicit `IntoIterator` impl always wins over the blanket one).
    fn analyze_for_in(&mut self, f: &HirForIn) -> Option<Vec<CheckedStmt>> {
        self.context.enter_scope();

        let iter_init = match self.classify_for_in_source(f) {
            Some(ForInSource::ToIterator(_checked, selected)) => {
                let old_bounds = self.bounds.len();
                self.bounds.push(selected);
                let result = self.synthesize_method_call(
                    HirPlaceRoot::Expr(Box::new(f.iterator.clone())),
                    "to_iterator",
                    f.span,
                );
                self.bounds.truncate(old_bounds);
                result
            }
            Some(ForInSource::DirectIterator(checked)) => Some(checked),
            None => None,
        };
        let ok = iter_init.is_some();

        let result = ok.then(|| {
            let iter_init = iter_init.expect("checked by `ok` above");
            let iter_id = self.resolver.fresh_synthetic_id();
            let iter_type = iter_init.r#type.clone();
            // `mut` -- `$iter.next()` takes `*mut self`, and `next`'s own
            // receiver auto-refs `$iter` itself now that `to_iterator`
            // returns an owned value (not a pointer): a mutable pointer can
            // only ever be taken to a binding actually declared `mut` (see
            // `VarBinding::mutable`). Harmless for the (rarer) case where
            // `iter_type` is itself already a `spec *mut Iterator<T>`
            // dynamic-dispatch handle -- the pointer *value* still never
            // gets reassigned, this only affects whether one could be.
            self.declare_binding(
                iter_id,
                f.span,
                &Ident("$iter".to_string()),
                Origin::default(),
                iter_type.clone(),
                Storage::Local,
                true,
            );
            let (iter_decl, iter_assign) =
                Self::synthetic_declaration(iter_id, f.span, "$iter", iter_type.clone(), iter_init);

            let old_bounds = self.bounds.len();
            if let Ok(conformances) = self.resolver.conformances_for_type(&iter_type) {
                self.bounds.extend(
                    conformances
                        .into_iter()
                        .map(|conform| (conform.target, conform.spec, conform.spec_args)),
                );
            }
            let while_stmt = self.analyze_for_in_loop(f);
            self.bounds.truncate(old_bounds);
            let while_stmt = while_stmt?;
            Some(vec![iter_decl, iter_assign, while_stmt])
        });

        let scope = self.context.leave_scope();
        self.warn_unused_bindings(scope, false);
        result.flatten()
    }


    /// Probes `f.iterator`'s own type once (single analysis -- reused
    /// directly as `$iter`'s own initializer in the `DirectIterator` case,
    /// unlike the `ToIterator` case, which still has `synthesize_method_call`
    /// analyze the identical expression a second time, embedded as
    /// `to_iterator`'s own receiver: there is no lower-level "resolve a
    /// method call against an already-checked receiver" primitive to hand
    /// it off to instead). This probe's own diagnostics are only kept when
    /// it fails outright (a genuine type error in `f.iterator` itself,
    /// which is the real problem and would otherwise be silently
    /// swallowed); otherwise they're discarded (truncated back to their
    /// pre-probe length) so nothing this analyzes twice (a `reveal` bypass,
    /// say) warns twice, and so a rejected `DirectIterator` candidate that
    /// falls through to `ToIterator`'s own (separate, real) analysis of the
    /// same expression doesn't warn twice either.
    fn classify_for_in_source(&mut self, f: &HirForIn) -> Option<ForInSource> {
        let errors_before = self.errors.len();
        let warnings_before = self.warnings.len();
        let Some(checked) = self.analyze_expr(&f.iterator, None) else {
            // A genuine type error in `f.iterator` -- keep it; it's the
            // real problem, and re-analyzing it (`ToIterator`'s own path,
            // or a second attempt here) would only reproduce it anyway.
            return None;
        };
        self.errors.truncate(errors_before);
        self.warnings.truncate(warnings_before);

        let to_iterator = self.for_in_conformances(&checked.r#type, "ToIterator");
        if !to_iterator.is_empty() {
            let expected_element = f.binding_type.as_ref().and_then(|raw| {
                self.resolve_type_or_error(f.id, f.span, raw, true)
            });
            // Kept before filtering: both failure paths below report what the
            // source *does* offer, which is the only actionable part of
            // either message.
            let available: Vec<ResolvedType> = to_iterator
                .iter()
                .filter_map(|conform| conform.spec_args.first().cloned())
                .collect();
            let candidates: Vec<_> = to_iterator
                .into_iter()
                .filter(|conform| {
                    expected_element.as_ref().is_none_or(|expected| {
                        conform.spec_args.first() == Some(expected)
                    })
                })
                .collect();
            if candidates.len() == 1 {
                let conform = candidates.into_iter().next().expect("length checked");
                return Some(ForInSource::ToIterator(
                    checked,
                    (conform.target, conform.spec, conform.spec_args),
                ));
            }
            // Zero candidates is only reachable *with* an annotation (an
            // unannotated loop filters nothing), and it is a mismatch, not an
            // ambiguity -- reporting it as "ambiguous" printed an empty
            // candidate list, naming neither what was asked for nor what
            // exists.
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

    /// The `while true { $next := $iter.next(); match $next { ... } }`
    /// portion of `analyze_for_in` -- split out so its own scope
    /// (`$next`, and each match arm's narrowing) can be entered/left
    /// independently of the outer `$iter` scope `analyze_for_in` itself
    /// owns.
    fn analyze_for_in_loop(&mut self, f: &HirForIn) -> Option<CheckedStmt> {
        let while_id = self.resolver.fresh_synthetic_id();
        self.loop_stack.push(while_id);
        self.context.enter_scope();

        let iter_read = HirPlaceRoot::Path(ExprPath::from(Ident("$iter".to_string())));
        let next = self.synthesize_method_call(iter_read, "next", f.span);

        let body = next.and_then(|next| {
            let next_id = self.resolver.fresh_synthetic_id();
            let next_type = next.r#type.clone();
            let ResolvedType::Enum {
                cell: option_cell, ..
            } = next_type.clone()
            else {
                // `core::iterator::Iterator::next` is declared to return
                // `Option<T>` (an ordinary enum), never a `spec *T` or
                // anything else -- if this doesn't hold, `core::iterator`
                // itself was edited inconsistently with this function.
                unreachable!("Iterator::next's declared return type is always an Option<T> enum");
            };
            self.declare_binding(
                next_id,
                f.span,
                &Ident("$next".to_string()),
                Origin::default(),
                next_type.clone(),
                Storage::Local,
                false,
            );
            let (next_decl, next_assign) =
                Self::synthetic_declaration(next_id, f.span, "$next", next_type.clone(), next);

            let mut tag_projections = Vec::new();
            let tag_type = self.resolve_field_projection(
                f.id,
                f.span,
                &mut tag_projections,
                &next_type,
                &Ident("tag".to_string()),
                &mut false,
            )?;
            let tag_read = CheckedExprNode {
                id: f.id,
                span: f.span,
                r#type: tag_type.clone(),
                kind: CheckedExpr::Place(CheckedPlace {
                    root: CheckedPlaceRoot::Variable {
                        decl_id: next_id,
                        storage: Storage::Local,
                        r#type: next_type,
                    },
                    projections: tag_projections,
                    r#type: tag_type.clone(),
                }),
            };

            let none_arm =
                self.for_in_none_arm(f, while_id, next_id, &option_cell, &tag_type, &tag_read);
            let some_arm = self.for_in_some_arm(f, next_id, &option_cell, &tag_type, tag_read);

            let match_expr = CheckedStmt::Expression(CheckedExprNode {
                id: f.id,
                span: f.span,
                // Never read -- this `match` is only ever used as a bare
                // statement (both arms fall through or `break`), so its
                // own result type is a don't-care placeholder, the same
                // way a `while`/`for` body's own tail value already is.
                r#type: ResolvedType::Void,
                kind: CheckedExpr::Match(CheckedMatch {
                    arms: vec![none_arm?, some_arm?],
                    else_branch: None,
                }),
            });

            Some(CheckedBlock {
                stmts: vec![next_decl, next_assign, match_expr],
                tail: None,
            })
        });

        self.context.leave_scope();
        self.loop_stack.pop();

        Some(CheckedStmt::While(CheckedWhile {
            id: while_id,
            span: f.span,
            condition: CheckedExprNode {
                id: while_id,
                span: f.span,
                r#type: ResolvedType::Bool,
                kind: CheckedExpr::Bool(true),
            },
            body: body?,
        }))
    }

    /// `Option::None => { break; }`.
    fn for_in_none_arm(
        &mut self,
        f: &HirForIn,
        while_id: HirId,
        next_id: HirId,
        option_cell: &Rc<RefCell<ResolvedEnumType>>,
        tag_type: &ResolvedType,
        tag_read: &CheckedExprNode,
    ) -> Option<CheckedMatchArm> {
        self.context.enter_scope();
        let refined = ResolvedType::Enum {
            cell: option_cell.clone(),
            variant: Some(0),
        };
        self.declare_narrowed_binding(
            next_id,
            f.span,
            &Ident("$next".to_string()),
            Origin::default(),
            refined,
            Storage::Local,
            false,
        );
        let body = CheckedBlock {
            stmts: vec![CheckedStmt::Break(CheckedBreak {
                id: self.resolver.fresh_synthetic_id(),
                span: f.span,
                loop_id: while_id,
            })],
            tail: None,
        };
        let scope = self.context.leave_scope();
        self.warn_unused_bindings(scope, false);

        let condition =
            Self::tag_equals(f, tag_type, tag_read, option_cell.borrow().variants[0].tag);
        Some(CheckedMatchArm {
            conditions: vec![vec![condition]],
            body,
        })
    }

    /// `Option::Some => { <mut>? binding := $next.value; ...body... }`.
    fn for_in_some_arm(
        &mut self,
        f: &HirForIn,
        next_id: HirId,
        option_cell: &Rc<RefCell<ResolvedEnumType>>,
        tag_type: &ResolvedType,
        tag_read: CheckedExprNode,
    ) -> Option<CheckedMatchArm> {
        self.context.enter_scope();
        let refined = ResolvedType::Enum {
            cell: option_cell.clone(),
            variant: Some(1),
        };
        self.declare_narrowed_binding(
            next_id,
            f.span,
            &Ident("$next".to_string()),
            Origin::default(),
            refined.clone(),
            Storage::Local,
            false,
        );

        let result = (|| {
            let mut value_projections = Vec::new();
            let value_type = self.resolve_field_projection(
                f.id,
                f.span,
                &mut value_projections,
                &refined,
                &Ident("value".to_string()),
                &mut false,
            )?;
            let value_read = CheckedExprNode {
                id: f.id,
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

            self.declare_binding(
                f.id,
                f.span,
                &f.binding,
                Origin::default(),
                value_type.clone(),
                Storage::Local,
                f.mutable,
            );
            let (binding_decl, binding_assign) = Self::synthetic_declaration(
                f.id,
                f.span,
                f.binding.as_ref(),
                value_type,
                value_read,
            );

            let user_stmts = self.analyze_stmts(&f.body.stmts)?;
            let user_tail = match &f.body.tail {
                Some(t) => Some(Box::new(self.analyze_expr(t, None)?)),
                None => None,
            };

            let mut stmts = vec![binding_decl, binding_assign];
            stmts.extend(user_stmts);
            Some(CheckedBlock {
                stmts,
                tail: user_tail,
            })
        })();

        let scope = self.context.leave_scope();
        self.warn_unused_bindings(scope, false);

        let condition =
            Self::tag_equals(f, tag_type, &tag_read, option_cell.borrow().variants[1].tag);
        Some(CheckedMatchArm {
            conditions: vec![vec![condition]],
            body: result?,
        })
    }

    /// `tag_read == <variant's own constant tag>` -- shared by both of
    /// `analyze_for_in`'s hand-built match arms.
    fn tag_equals(
        f: &HirForIn,
        tag_type: &ResolvedType,
        tag_read: &CheckedExprNode,
        tag: NumberValue,
    ) -> CheckedExprNode {
        let tag_const = CheckedExprNode {
            id: f.id,
            span: f.span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Number(tag),
        };
        CheckedExprNode {
            id: f.id,
            span: f.span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(tag_read.clone()),
                right: Box::new(tag_const),
            }),
        }
    }

    /// Builds `root.method()` as ordinary, source-shaped HIR (fresh
    /// synthetic ids throughout) and analyzes it exactly like a
    /// hand-written call -- auto-ref, overload resolution, and static-vs-
    /// dynamic-dispatch selection all Just Work, unreimplemented, the same
    /// way they would for `x.method()` written by a user. `root` is
    /// `HirPlaceRoot::Expr` for a receiver that's itself an arbitrary
    /// expression (evaluated exactly once, as part of this call), or
    /// `HirPlaceRoot::Path` for a receiver that's a synthetic local
    /// already declared by name (`$iter`) -- see `HirPlaceRoot`'s own doc
    /// comment for why those are the only two shapes a place root has.
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

    /// The `CheckedStmt::Declaration` + `CheckedStmt::Expression(Assignment)`
    /// pair every synthetic `name := value;` in `analyze_for_in` needs --
    /// exactly `analyze_walrus`'s own shape, just built from an
    /// already-`CheckedExprNode` value instead of lowering one from HIR
    /// (there's no HIR here to lower from -- `value` was already produced
    /// by `synthesize_method_call`/a hand-built field read).
    fn synthetic_declaration(
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
            id,
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
