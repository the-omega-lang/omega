use super::*;

impl<'r> Analyzer<'r> {
    /// `expected` is the concrete type this expression's *result* is about
    /// to flow into, when the caller has one available (a declaration's
    /// annotated type, an assignment's target, a `return`'s function
    /// signature, a call argument's parameter, a struct/union field's
    /// declared type, ...) -- `None` everywhere else. Only a handful of
    /// forms actually consult it: an unsuffixed number literal adapts to it
    /// (untyped-constant inference -- see `default_or_expected_number_type`),
    /// and array/`if`/block/`-`/binary-operator forms thread it further down
    /// into whichever of their own sub-expressions could themselves be
    /// unsuffixed literals. Everything else ignores it entirely -- this is
    /// deliberately *not* full bidirectional inference, just enough top-down
    /// context for a literal whose own type isn't pinned by an explicit
    /// suffix to adapt instead of defaulting to i32/f64.
    ///
    /// Every form with any real work of its own gets a named method below;
    /// the arms that stay inline here are the ones whose whole analysis *is*
    /// "this literal has this type".
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

            // A string literal is a `*str` -- a UTF-8 byte run with a
            // compile-time-known length and no null terminator (see
            // `ResolvedType::Str`). Its bytes are raw UTF-8, not decoded
            // characters, unlike `*char`. Immutable, like every literal.
            HirExpr::String(s) => literal(
                ResolvedType::Str { mutable: false },
                CheckedExpr::String(s.0.clone()),
            ),

            // `b"..."` -- a raw byte run with a compile-time-known length,
            // not a null-terminated C string: `*[?]u8` (see `Context::
            // resolve_pointer_type`'s `UnknownSizeArray` case), never `*u8`.
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
            HirExpr::FunctionCall(call) => self.analyze_call(id, span, call),
            HirExpr::Assignment(assignment) => self.analyze_assignment(id, span, assignment),
            HirExpr::CompoundAssign(HirCompoundAssign { target, op, value }) => {
                self.analyze_compound_assign(id, span, target, *op, value)
            }
            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                self.analyze_address_of(id, span, base, *mutable, expected)
            }
            HirExpr::Negate(base) => self.analyze_negate(id, span, base, expected),
            HirExpr::BitNot(base) => self.analyze_bit_not(id, span, base, expected),
            HirExpr::Increment(base) => self.analyze_incr_decr(id, span, base, BinaryOp::Add),
            HirExpr::Decrement(base) => self.analyze_incr_decr(id, span, base, BinaryOp::Sub),
            HirExpr::BinaryOp(bin) => self.analyze_binary_expr(id, span, bin, expected),
            HirExpr::ArrayLiteral(elements) => {
                self.analyze_array_literal(id, span, elements, expected)
            }
            HirExpr::StructLiteral(lit) => self.analyze_struct_literal(id, span, lit, expected),
            HirExpr::Match(m) => self.analyze_match(id, span, m),
            HirExpr::Cast(HirCast { target, base }) => self.analyze_cast(id, span, target, base),

            // Reached only when *not* wrapped in `&`/`&mut` (see
            // `analyze_address_of`, which intercepts the `&`-wrapped shape):
            // a slice expression alone can't say whether an immutable or a
            // mutable slice was meant, so it's never valid on its own.
            HirExpr::Slice(_) => {
                self.error(id, span, AnalysisErrorKind::SliceRequiresAddressOf);
                None
            }

            // Reached only when a standalone range didn't get intercepted
            // by `Analyzer::analyze_for`'s own dedicated handling first --
            // see `HirExpr::Range`'s doc comment.
            HirExpr::Range(_) => {
                self.error(id, span, AnalysisErrorKind::RangeNotAllowedHere);
                None
            }
        }
    }

    /// An ordinary *read* of a place. This is the only path a read takes:
    /// an assignment's own target is resolved separately (see
    /// `require_mutable_place`'s `mark_written` for the write side), while a
    /// compound assignment's or increment's synthesized read component
    /// desugars to a `HirExpr::Place` and arrives back here.
    fn analyze_place_read(
        &mut self,
        id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (checked_place, r#type, _mutable) = self.analyze_place(id, span, place, expected)?;
        // This is the one place an ordinary *read* of a place
        // happens (an assignment's own target is resolved
        // separately, never through here -- see
        // `require_mutable_place`'s own `mark_written` call for the
        // write side) -- including a compound-assign/increment's
        // synthesized read component, since those desugar to a
        // `HirExpr::Place` of their own.
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked_place.root {
            self.context.mark_used(decl_id);
        }
        // A `comp` binding carries no storage -- every read substitutes its
        // already-known value directly, so this never reaches MIR lowering/
        // codegen as a `Storage::Comp` place at all (see `Storage::Comp`'s
        // doc comment). Any projections (`comp_struct.field`, `comp_arr[i]`,
        // ...) are applied directly against the already-known `ConstValue`
        // -- see `apply_comp_projection`.
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

    /// Resolves a `Storage::Comp` place root's already-known value and
    /// applies every remaining projection against it -- the shared
    /// substitution logic every comp-binding read site needs identically
    /// (`analyze_place_read`, `analyze_address_of`, and a comp-binding
    /// method receiver in `calls::adapt_self_argument`).
    ///
    /// Also records `checked_place`'s field/variant usage
    /// (`crate::dead_code`): every one of those call sites collapses
    /// `checked_place` into a bare `CheckedExpr::Const` (or discards it
    /// entirely), so it would otherwise never reach
    /// `crate::dead_code::collect_module`'s whole-program usage walk --
    /// exactly the same reasoning `eval_comp`'s own doc comment explains
    /// for a `comp <expr>`'s subtree.
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
        // A *local* comp binding's value lives in this throwaway
        // `Analyzer`'s own `Context`; a top-level one's was recorded by a
        // *different* `Analyzer` (the one that resolved its own
        // `HirItem::Walrus`) and only survives in the driver's cross-item
        // state -- see `ModuleResolver::resolve_comp_value`.
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

    /// Applies one projection to an already-known `comp` value, producing
    /// the projected-into `ConstValue` -- the analyzer-side counterpart of
    /// `comp_eval::Interpreter`'s own (near-identical) `read_projection`,
    /// kept as its own implementation rather than shared: this one reports
    /// through `Analyzer::error`/`AnalysisErrorKind::CompEvalFailed`
    /// (matching every other diagnostic in this module), while the
    /// interpreter's uses its own `CompError`/call-trace machinery -- the
    /// two error-reporting conventions don't unify cheaply, so a small
    /// amount of duplicated *logic* (not the value itself) is the simpler
    /// tradeoff. `Index`'s own index expression is evaluated via
    /// `eval_comp` -- an out-of-range or non-`comp`-evaluable index is
    /// exactly as much a "not evaluable at compile time" failure as
    /// anything else here.
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
            // No real memory for a `comp` value to dereference through --
            // the interpreter's own `Deref` handling has the identical
            // restriction (see `comp_eval::Interpreter::read_projection`),
            // for the identical reason.
            CheckedProjection::Deref { .. } => unsupported(
                self,
                "dereferencing a pointer inside a 'comp' binding projection isn't supported yet",
            ),
            // A `spec *Self` value has no `ConstValue` shape at all --
            // dynamic dispatch isn't comp-evaluable in the first place (see
            // `docs/19-compile-time-evaluation.md`'s "What it can't (yet)"),
            // so `.ptr`/`.vtable` can never actually see a real base value
            // here; reject uniformly rather than reach the fallback panic.
            CheckedProjection::SpecObjectPtr { .. } | CheckedProjection::SpecObjectVtable => {
                unsupported(
                    self,
                    "accessing a spec object's pointer/vtable inside a 'comp' evaluation isn't supported",
                )
            }
        }
    }

    /// `reveal base` -- fully transparent: this produces exactly what
    /// analyzing `base` alone would (this node's own `id`/`span` are
    /// discarded in favor of `base`'s), with a `reveal_stack` frame pushed
    /// around it. See `check_visibility`/`reveal_stack`.
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

    /// `comp base` -- evaluates `base` at compile time (see
    /// `docs/19-compile-time-evaluation.md`). `base` is analyzed completely
    /// ordinarily first -- full type-checking, generic/overload/cross-
    /// module resolution, exactly as if `comp` weren't there at all -- so
    /// this needs no type-checking logic of its own; only the resulting,
    /// already-checked tree is handed to the interpreter
    /// (`crate::comp_eval`). On success the whole node collapses into
    /// `CheckedExpr::Const`, exactly like an ordinary literal, so nothing
    /// downstream of analysis (MIR lowering, codegen) ever needs to know a
    /// value came from `comp` at all.
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

    /// Interprets an already-checked `expr` at compile time, reporting a
    /// precise diagnostic (naming the exact blocking construct, plus the
    /// call-site trace -- see `comp_eval::CompError`) and returning `None`
    /// on failure. Shared by `analyze_comp` itself and a `comp`-bound
    /// binding's own initializer, which needs exactly the same evaluate-or-
    /// diagnose step regardless of whether its right-hand side happened to
    /// carry its own explicit leading `comp` too (if it did, this simply
    /// interprets an already-`CheckedExpr::Const` node, an immediate,
    /// trivial success -- see `comp_eval::Interpreter::eval_expr`'s own
    /// `Const` arm).
    ///
    /// `expr` is about to collapse into (or be discarded in favor of) a
    /// bare `CheckedExpr::Const` at every one of this method's call sites,
    /// which would otherwise silently erase any field access/enum
    /// construction it contains from `crate::dead_code`'s whole-program
    /// usage walk (that walk only ever sees the final, persisted tree) --
    /// recording `expr`'s usage here, unconditionally, before it's gone,
    /// is what keeps a field/variant touched only inside a `comp`
    /// evaluation from false-positiving as unused/never-constructed. Done
    /// regardless of whether interpretation below actually succeeds: the
    /// access is real in the source either way, and a failed evaluation
    /// already hard-errors the compile on its own.
    pub(super) fn eval_comp(
        &mut self,
        id: HirId,
        expr: &CheckedExprNode,
    ) -> Option<crate::resolved_type::ConstValue> {
        crate::dead_code::collect_expr(expr, &mut self.field_usage);
        match crate::comp_eval::eval(self.resolver, expr) {
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

    /// An `if`/`else if`/`else` chain used as an expression.
    fn analyze_if(
        &mut self,
        node_id: HirId,
        span: Span,
        branches: &[(HirExprNode, HirBlock)],
        else_branch: Option<&HirBlock>,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // No `else` at all forces `Void` regardless of branch
        // content below (the "implicit else" is `{}`, matching
        // Rust's identical rule for a possibly-skipped `if`) --
        // branches get no expected type threaded into them in that
        // case, exactly as if this whole feature didn't exist for
        // them: there's no cross-branch value to unify toward.
        let has_else = else_branch.is_some();

        // Earliest-wins unification: branch 0 is always the
        // *anchor* -- the incoming `expected`, if any, otherwise
        // branch 0's own (widened) type once it's analyzed -- and
        // every other branch/`else` is checked *against* that
        // anchor, never the other way around. Unlike the old
        // "peek every branch, use whichever non-literal one is
        // found first" approach, this never lets a *later* branch's
        // already-fixed type (an explicit suffix, a variable, ...)
        // retroactively decide what an earlier adaptable literal
        // infers to -- a later branch only has to *agree* with the
        // anchor (see the mismatch check below), never supply it.
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

        // What the whole `if` resolves to: the first concrete
        // (non-diverging) type among the branches and the `else`,
        // if any -- diverging branches (ending in `return`) are
        // wildcards, exempt from the check below.
        let branch_kinds: Vec<Option<ResolvedType>> = checked_branches
            .iter()
            .map(|(_, b)| Self::block_type(b))
            .collect();
        let else_kind: Option<Option<ResolvedType>> = checked_else.as_ref().map(Self::block_type);

        // Widened: branches producing *different variants* of one
        // enum (`if c { E::A } else { E::B }`) still agree on the
        // enum itself, which is then the whole `if`'s type.
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

    /// An ordinary call, after the four interceptors (overloaded,
    /// overloaded-static, generic, generic-static) have each declined it.
    fn analyze_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Option<CheckedExprNode> {
        // Tried in priority order; the first to claim the call answers it.
        let interceptors: [Interceptor<'r>; 5] = [
            Self::resolve_spec_qualified_call,
            Self::resolve_overloaded_call,
            Self::resolve_overloaded_static_call,
            Self::resolve_generic_call,
            Self::resolve_generic_static_call,
        ];
        for intercept in interceptors {
            if let Intercepted::Claimed(result) = intercept(self, node_id, span, call) {
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
            // Overload resolution already fully analyzed (and
            // type-checked, including untyped-constant adaptation)
            // every user-written argument itself, to score
            // candidates -- redoing that here would risk
            // double-erroring, and can't change the outcome anyway.
            Some(overload_args) => args.extend(overload_args),
            None => {
                // The counts shown to the user exclude an implicit
                // `self` (at this point `args` holds exactly that,
                // and nothing else) -- the user never wrote it, so
                // "takes 1 argument but 2 were supplied" for a 1-arg
                // method call would only confuse.
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

    /// `target = value`.
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
        // field-visibility checks -- see `strip_reveal`'s doc
        // comment for why `reveal` on an assignment's target (`reveal
        // a.b = c;`) never reaches `analyze_expr`'s own `HirExpr::
        // Reveal` arm at all (it wraps only `target`, never the whole
        // `Assignment`).
        let (checked_target, target_type, target_mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(target.id, target.span, place, None)
            })?;
        self.require_mutable_place(node_id, span, &place.root, &checked_target, target_mutable)?;

        // Resolved *before* the value, unlike almost everywhere else
        // in this match -- the target's own type is exactly the
        // expected type an unsuffixed literal value should adapt to.
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

    /// `&base`/`&mut base`, including the two shapes that aren't pointers
    /// at all: `&base[range]` (a slice) and `&[...]` (a compile-time slice).
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
        // pointer; see `analyze_slice` for why this is the *only* way to
        // produce one. Both this and the compile-time-slice form below run
        // under `was_reveal` exactly like the plain-place form further down:
        // a stripped `reveal` has to keep its bypass at *every* operand
        // position, not just the one that happens to reach `analyze_place`
        // directly.
        if let HirExpr::Slice(HirSlice {
            base: slice_base,
            range,
        }) = &base.expr
        {
            return self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_slice(node_id, span, slice_base, range, mutable)
            });
        }
        // `&[...]`/`&mut [...]` -- a compile-time slice, not an ordinary
        // place; see `analyze_const_slice`.
        if let HirExpr::ArrayLiteral(elements) = &base.expr {
            return self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_const_slice(node_id, span, elements, mutable, expected)
            });
        }
        let HirExpr::Place(place) = &base.expr else {
            self.error(node_id, span, AnalysisErrorKind::AddressOfNotAPlace);
            return None;
        };
        // See `Analyzer::strip_reveal`'s doc comment -- same
        // reasoning as `HirExpr::Assignment`'s arm.
        let (checked_place, place_type, place_mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(base.id, base.span, place, None)
            })?;
        // `&comp_binding` -- `&mut` on one is impossible (a `comp` binding
        // is never `mutable`, so `require_mutable_place` below rejects it
        // with the same diagnostic any other immutable binding's `&mut`
        // gets -- deliberately *not* intercepted here), but plain `&` isn't
        // gated by mutability at all, so it's handled here: **const
        // promotion**, mirroring Rust's identical answer for `&SOME_CONST`
        // (see docs/19-compile-time-evaluation.md's "calling a method on a
        // `comp` binding" section). The binding's already-known value is
        // wrapped in `ConstValue::Ref` -- the exact same "address of a
        // separately-built piece of `comp` data" codegen already emits for
        // `&<place>` *inside* a `comp` evaluation (see `comp_eval::
        // Interpreter::eval_expr`'s `AddressOf` arm) -- so materializing it
        // into a real, addressable rodata blob is already-proven machinery,
        // just triggered from a new call site.
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
            // `&mut` requires write access, and -- unlike plain `&`
            // below -- *always* produces a fully-widened pointee, no
            // exception: the only way a mutable refined pointer can
            // ever exist is a `match`-narrowed *view* of an
            // already-mutable place, never something freshly minted
            // here (see `ResolvedType::accepts`'s doc comment for why
            // that distinction is what keeps a mutable pointer/slice
            // from ever needing to widen implicitly).
            self.require_mutable_place(node_id, span, &place.root, &checked_place, place_mutable)?;
            // De-assumption: a writable alias to this place now
            // exists, so any later direct read of it (in this or an
            // enclosing scope) can no longer trust a narrower type
            // than the plain one -- this is the actual "de-assume a
            // proof once a mutable reference has been taken" step.
            if let Some((ident, ..)) = self.narrowable_place(place) {
                self.context.widen_variable(&ident);
            }
            place_type.widened()
        } else {
            // A variant refinement surviving `&` is only sound when
            // it's a *permanent* fact about the place -- its own
            // declared/inferred type (`a := Entity::Person { ... }`;
            // reassigning a different variant to `a` later is already
            // rejected by `ResolvedType::accepts`, so a pointer into
            // it can never go stale that way). A `match`-narrowed
            // shadow's refinement is only true for that one arm's
            // lexical scope -- the underlying storage can still hold
            // a different variant once the arm ends -- so that case
            // still widens, exactly as before this distinction
            // existed (see `VarBinding::narrowed`).
            let narrowed_shadow = self.narrowable_place(place).is_some_and(|(ident, ..)| {
                self.context
                    .find_variable(&ident)
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

    /// Unary `-`. `expected` passes straight through: negation is
    /// transparent to its own result type, so this node's own type context
    /// is exactly right for `base` too (notably, `-100` is exactly as
    /// adaptable as `100`).
    fn analyze_negate(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // `expected` passes straight through -- `Negate` is
        // transparent to its own result type (it's always exactly
        // `base`'s type, see below), so whatever type context this
        // node itself received is exactly the right context for
        // `base` too (including, notably, an unsuffixed literal
        // `base` -- `-100` is exactly as adaptable as `100`).
        let checked_base = self.analyze_expr(base, expected)?;
        // Signed ints and floats only -- matching Rust, unary `-` on
        // an unsigned integer (or `bool`/`char`, neither of which is
        // numeric at all) is rejected rather than silently wrapping.
        let negatable = matches!(
            checked_base.r#type.numeric_kind(),
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

    /// Unary `~`, transparent to its own result type exactly like
    /// `analyze_negate` -- except, unlike `analyze_negate`, `char`/pointer
    /// operands first coerce to their `arithmetic_repr` (see
    /// `Self::coerce_for_unary_op`), so `~some_char` is legal and produces a
    /// `u32`. `bool` is deliberately not given the same treatment: unlike
    /// `& | ^` (native on `bool`, see `analyze_binary_op`), bitwise-NOT of
    /// `bool`'s `0`/`1` representation doesn't stay within `{0,1}` (`~0u8 ==
    /// 255`), so there is no sound native meaning for it to have.
    fn analyze_bit_not(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirExprNode,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // `expected` passes straight through, same reasoning as
        // `Negate`'s arm just above -- `~` is transparent to its own
        // result type.
        let checked_base = self.analyze_expr(base, expected)?;
        let checked_base = Self::coerce_for_unary_op(checked_base);
        let bitnotable = matches!(
            checked_base.r#type.numeric_kind(),
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

    /// Resolves both operands, then checks the operator itself (see
    /// `analyze_binary_op`).
    fn analyze_binary_expr(
        &mut self,
        node_id: HirId,
        span: Span,
        bin: &omega_hir::HirBinaryOp,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // Two composed inference rules, mirroring precedent that
        // already exists elsewhere rather than inventing a new
        // philosophy: (1) the outer `expected` this node itself
        // received (e.g. from an enclosing assignment, whose target
        // is already resolved by the time its value is analyzed)
        // flows to both operands, but only for a non-comparison op
        // -- an arithmetic/bitwise result *is* its operand type, so
        // this is sound, but a comparison's result is always `bool`
        // regardless of its (numeric) operands, so threading a
        // `bool` expectation into them would be nonsensical.
        // (2) Left is always analyzed first and, absent an outer
        // `expected`, its own resolved (and `.widened()`, matching
        // `HirExpr::If`'s identical anchor treatment) type becomes
        // `expected` for the right operand -- the same "earliest
        // operand is the anchor" rule `if`-expression branches
        // already commit to, applied here instead of a fuller
        // "peek every position" search. Safe either way `expected`
        // ends up wrong for a given operand: it's consulted only by
        // genuinely adaptable things (a bare literal); anything
        // already concretely typed ignores it, and `analyze_binary_op`
        // below still independently enforces exact operand-type
        // equality on the results, so this can only turn a
        // previously-failing narrowing case into a working one, never
        // weaken a real mismatch.
        let operand_expected = if bin.op.is_comparison() {
            None
        } else {
            expected
        };
        let checked_left = self.analyze_expr(&bin.left, operand_expected)?;
        // For a non-comparison op, anchor to what `left` will *become*
        // (`arithmetic_repr`), not what it currently is -- otherwise
        // `some_char + 1` fails to compile, since the bare `1` would anchor
        // to `char` (not itself numeric, so it falls back to its own
        // default `i32`) while `left` coerces to `u32` in
        // `analyze_binary_op` below, and the two would then mismatch. A
        // comparison never anchors this way: `char` doesn't coerce for a
        // comparison at all (see `Self::coerce_for_binary_op`), and a
        // pointer has no adaptable bare-literal form to anchor in the first
        // place, so there's nothing to gain there.
        let mut left_type = checked_left.r#type.widened();
        if !bin.op.is_comparison() {
            left_type = left_type.arithmetic_repr().unwrap_or(left_type);
        }
        let checked_right = self.analyze_expr(&bin.right, operand_expected.or(Some(&left_type)))?;
        self.analyze_binary_op(node_id, span, bin.op, checked_left, checked_right)
    }

    /// `<Target>base`.
    fn analyze_cast(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &Type,
        base: &HirExprNode,
    ) -> Option<CheckedExprNode> {
        let target_type = self.resolve_type_or_error(node_id, span, target, true)?;
        // `base` keeps its own natural (default, unsuffixed-literal)
        // type -- the cast's target is an explicit instruction to
        // convert, never context to infer `base`'s own type from
        // (`<f32>10` casts a genuine i32 `10` to `f32`, it doesn't
        // just relabel an already-f32 literal).
        let checked_base = self.analyze_expr(base, None)?;

        // Generalized over `Pointer`/`Slice`/`Str` alike (see
        // `ResolvedType::pointer_like_mutable`'s doc comment) --
        // stays first and unconditional, before either cast-kind
        // path below, so e.g. `<*mut str>` on an immutable `*str`
        // is caught here rather than silently succeeding as a
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

        // `<spec *Spec>base` -- explicit dynamic-dispatch coercion. A
        // third family, genuinely separate from both the numeric path and
        // the byte-pointer family below (`SpecObject` has no `cast_class`
        // either): unlike every other cast here, this one can only ever
        // succeed by *proving* something (`pointee` genuinely implements
        // `spec<type_args>`), not by a pure width/signedness computation,
        // so it's checked and returned immediately rather than folding
        // into `cast_kind`'s three-way `if`/`else`. Reuses exactly the
        // same proof `coerce_to_expected` already runs for the *implicit*
        // version of this same coercion (see its own doc comment) --
        // explicit casting was previously the one direction that never
        // worked at all (only 4 implicit-coercion sites did; see
        // `docs/08-specs.md`'s "Coercion into `spec *T`" caveat).
        if let ResolvedType::SpecObject {
            spec,
            type_args,
            mutable,
        } = &target_type
        {
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

        // The str/byte-slice family (`*str`/`*[?]u8`/`*[?]i8`) and the
        // sized-array-to-slice widening just below are both tried first:
        // fat pointers don't fit `cast_class`'s scalar-width model at all
        // (`Str`/`Slice` both return `None` from it), so both are
        // genuinely separate machinery, not an extension of the numeric
        // path below.
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
                (checked_base.r#type.cast_class(), target_type.cast_class())
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

    /// `++base`/`--base`: validates `base` is a place (like `AddressOf`) of
    /// a numeric type, then desugars directly into `base = base <op> 1` --
    /// an ordinary `Assignment` wrapping a `BinaryOp` over `base`'s own
    /// place and a `Number` matching its exact resolved type/kind. Building
    /// the literal `1` here, rather than going through the parser's
    /// `NumberExpr`/`HirExpr::Number` path, is what lets this work for any
    /// numeric type (an untyped `1` in source would default to `i32`, which
    /// would then fail `BinaryOp`'s "operands must match exactly" rule for
    /// every other numeric type) -- analysis already knows `base`'s exact
    /// type here, so it can build a same-typed constant directly.
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
        // See `Analyzer::strip_reveal`'s doc comment -- same reasoning as
        // `HirExpr::Assignment`'s arm.
        let (checked_place, place_type, mutable) =
            self.with_reveal_bypass(was_reveal, node_id, span, |this| {
                this.analyze_place(base.id, base.span, place, None)
            })?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let Some(kind) = place_type.numeric_kind() else {
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

    /// The type-checking core of `left op right`, shared by `HirExpr::
    /// BinaryOp`'s arm and `analyze_compound_assign`'s desugaring (`target
    /// op= value` -> `target = target op value`) -- both already have their
    /// operands analyzed (a compound assignment's `left` is a synthetic
    /// place-read, never itself re-analyzed here), so this only ever
    /// type-checks and combines two already-`CheckedExprNode`s.
    fn analyze_binary_op(
        &mut self,
        node_id: HirId,
        span: Span,
        op: BinaryOp,
        checked_left: CheckedExprNode,
        checked_right: CheckedExprNode,
    ) -> Option<CheckedExprNode> {
        // Coerce a `char`/pointer operand to its `arithmetic_repr` first --
        // see `Self::coerce_for_binary_op`'s doc comment for exactly which
        // op/type combinations coerce. Everything below this line only ever
        // sees the coerced types, so it needs no further special-casing for
        // either: a coerced `char`'s `u32`/a coerced pointer's `usize`
        // already has a real `numeric_kind`, same as any other operand.
        let checked_left = self.coerce_for_binary_op(op, checked_left);
        let checked_right = self.coerce_for_binary_op(op, checked_right);

        // `char` is comparable (it's `numeric_kind`-shaped underneath --
        // one unsigned 4-byte scalar value, ordered by codepoint), but
        // never arithmetic/bitwise: combining two `char`s that way can
        // produce a codepoint that isn't a valid Unicode scalar value at
        // all, and this language has no fallible/validating path for that
        // yet (see `ResolvedType::Char`'s doc comment). So `char` is
        // accepted here *only* for a comparison op (and, at this point,
        // never coerced -- see `coerce_for_binary_op`) -- everything else
        // still requires genuine `numeric_kind`.
        //
        // `bool` is closed under `== != & | ^` (any combination of valid
        // `bool`s -- `0`/`1` -- stays a valid `bool`), so those five get to
        // stay natively `bool`, no coercion at all: unlike `char`, there's
        // no soundness reason to leave `bool` for them. Arithmetic/shifts
        // still aren't offered (`true + true` has no meaning to fall back
        // on), and neither is `~` (see `analyze_bit_not`'s doc comment).
        for operand in [&checked_left, &checked_right] {
            let is_valid = operand.r#type.numeric_kind().is_some()
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

        // No implicit numeric conversions anywhere else in this
        // language (see e.g. `AssignmentTypeMismatch`) -- arithmetic
        // between two different numeric types is no exception.
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

        // No native float remainder instruction (see
        // `AnalysisErrorKind::FloatRemainder`'s doc comment) --
        // matching C, which requires `fmod`/`fmodf` instead of `%`.
        if op == BinaryOp::Rem
            && matches!(
                checked_left.r#type.numeric_kind(),
                Some(NumericKind::Float(_))
            )
        {
            self.error(node_id, span, AnalysisErrorKind::FloatRemainder);
            return None;
        }

        // No native float bitwise/shift instructions either -- same
        // reasoning as `Rem` just above, just for a whole family of ops
        // instead of one.
        if matches!(
            op,
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr
        ) && matches!(
            checked_left.r#type.numeric_kind(),
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

        // A comparison always produces `bool`, regardless of the
        // (still-numeric, still-matching) operand type; an
        // arithmetic op's result is that same operand type.
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

    /// Wraps `operand` in the implicit `Cast` its `arithmetic_repr` (see
    /// `ResolvedType::arithmetic_repr`) calls for, if it has one -- the same
    /// `Cast` a user would have to write by hand, built via the exact same
    /// `cast_class`/`resolve_cast_kind` pair `analyze_cast` itself uses, so
    /// this needs no codegen support of its own. A no-op (returns `operand`
    /// unchanged) for anything with no `arithmetic_repr` at all (every
    /// genuinely numeric type, `bool`, structs, ...).
    ///
    /// `char` only coerces for a non-comparison op: comparing two `char`s
    /// directly, uncoerced, is what lets codegen's `MirExpr::BinaryOp` arm
    /// keep special-casing `Char` as its own 4-byte unsigned scalar (see its
    /// existing comment there) instead of ever seeing a coerced `u32` pair
    /// that used to be `char`s. A pointer coerces unconditionally, including
    /// for a comparison -- which is what makes `*mut T == *T` type-check for
    /// free: both sides become a plain `usize`, so pointee type and
    /// mutability never enter the equality check at all.
    fn coerce_for_binary_op(&self, op: BinaryOp, operand: CheckedExprNode) -> CheckedExprNode {
        if op.is_comparison() && operand.r#type == ResolvedType::Char {
            return operand;
        }
        match operand.r#type.arithmetic_repr() {
            Some(repr) => Self::coerce_to(operand, repr),
            None => operand,
        }
    }

    /// `coerce_for_binary_op`'s unary counterpart, for `~` (see
    /// `analyze_bit_not`) -- unconditional, since there's no comparison/
    /// arithmetic distinction to make for a unary op.
    fn coerce_for_unary_op(operand: CheckedExprNode) -> CheckedExprNode {
        match operand.r#type.arithmetic_repr() {
            Some(repr) => Self::coerce_to(operand, repr),
            None => operand,
        }
    }

    /// The shared mechanics behind both `coerce_for_*_op` above: builds the
    /// same `CheckedExpr::Cast` an explicit `<repr>operand` would produce.
    /// `repr` is always itself numeric (every `arithmetic_repr` value is),
    /// so both `cast_class` calls below are infallible.
    fn coerce_to(operand: CheckedExprNode, repr: ResolvedType) -> CheckedExprNode {
        let source_class = operand
            .r#type
            .cast_class()
            .expect("arithmetic_repr's source always has a cast_class");
        let target_class = repr
            .cast_class()
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

    /// The operand's value as an `i128`, if it's a bare literal (`Number`
    /// or `Bool`) rather than a runtime-varying place/expression --
    /// `AlwaysTrueFalseComparison`'s "exactly one side is a literal" check
    /// reads this from both sides and keeps whichever one succeeds.
    fn literal_i128(expr: &CheckedExprNode) -> Option<i128> {
        match &expr.kind {
            CheckedExpr::Number(NumberValue::Signed(n)) => Some(*n as i128),
            CheckedExpr::Number(NumberValue::Unsigned(n)) => Some(*n as i128),
            CheckedExpr::Bool(b) => Some(*b as i128),
            CheckedExpr::Char(c) => Some(*c as i128),
            _ => None,
        }
    }

    /// A comparison whose truth value doesn't depend on its non-literal
    /// operand at all -- e.g. `unsigned_var < 0` (always false) or
    /// `unsigned_var >= 0` (always true) -- computed via plain bound
    /// arithmetic against the operand type's `integer_domain()`, the same
    /// domain `exhaustiveness::check` already treats as "the whole range" a
    /// match must cover. Only fires when exactly one side is a literal;
    /// both-literal or both-variable comparisons say nothing about the
    /// *type's* range and are out of scope here.
    fn check_always_true_false_comparison(
        &mut self,
        node_id: HirId,
        span: Span,
        op: BinaryOp,
        left: &CheckedExprNode,
        right: &CheckedExprNode,
    ) {
        let Some((lo, hi)) = left.r#type.integer_domain() else {
            return;
        };

        let (literal, literal_on_right) =
            match (Self::literal_i128(left), Self::literal_i128(right)) {
                (Some(l), None) => (l, false),
                (None, Some(r)) => (r, true),
                _ => return,
            };

        // `x op literal` if `literal_on_right`, else `literal op x` --
        // each arm picks the bound that pins the result to `true`, then
        // the one that pins it to `false`; anything left over genuinely
        // depends on `x`'s runtime value.
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

    /// `target op= value` -- desugars directly into `target = target op
    /// value`, the same pattern `analyze_incr_decr` already uses for
    /// `++`/`--` (a `BinaryOp` over a place-read and `value`, wrapped in an
    /// ordinary `Assignment`), generalized to a real right-hand side
    /// instead of a synthesized `1`. `value` is analyzed with `expected =
    /// Some(&target_type)` -- the same treatment a plain assignment's value
    /// already gets (`HirExpr::Assignment`'s arm) -- so `a *= 5` adapts an
    /// unsuffixed literal `5` to `a`'s own type rather than defaulting to
    /// `i32`/`f64` and then failing `analyze_binary_op`'s "operands must
    /// match exactly" check.
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
        // See `Analyzer::strip_reveal`'s doc comment -- same reasoning as
        // `HirExpr::Assignment`'s arm.
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

    /// Whether `target` may be cast into at all, given `source` -- `
    /// cast_class` gives `char`/`bool` a class so they can be cast *out* to
    /// any numeric type via the ordinary width/signedness rules below, but
    /// `resolve_cast_kind` has no notion of direction, so left alone that
    /// would symmetrically allow casting arbitrary integers *in* too, which
    /// isn't sound: not every `u32` is a valid Unicode scalar value, and
    /// there's no implicit "nonzero is true" here. Mirrors Rust's own `as`
    /// rules exactly: `u8 as char` is the one direction into `char` that's
    /// always valid (every byte is a valid codepoint); nothing else is,
    /// pending a real validating constructor (`char::from_u32`-equivalent)
    /// this compiler doesn't have yet. Nothing casts into `bool` at all.
    fn allows_cast_into(source: &ResolvedType, target: &ResolvedType) -> bool {
        match target {
            ResolvedType::Char => matches!(source, ResolvedType::Char | ResolvedType::U8),
            ResolvedType::Bool => *source == ResolvedType::Bool,
            _ => true,
        }
    }

    /// Picks the one `CastKind` a `(source, target)` `CastClass` pair needs,
    /// purely from width/signedness -- no per-type-pair table (see
    /// `CastClass`'s doc comment).
    fn resolve_cast_kind(source: CastClass, target: CastClass) -> CastKind {
        match (source, target) {
            (CastClass::Int { width: sw, signed }, CastClass::Int { width: tw, .. }) => {
                if sw == tw {
                    CastKind::Reinterpret
                } else if sw < tw {
                    // Widening reproduces the *source's* value, so it's the
                    // source's signedness that picks sign- vs zero-extend
                    // (matches Rust's `as`: `-1i8 as u32 == u32::MAX`).
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

    /// The str/byte-slice family's cast resolution -- a fat pointer
    /// (`Str`/`Slice{item:U8|I8}`) never has a `cast_class` (it doesn't fit
    /// the scalar-width model at all), so this is genuinely separate
    /// machinery from `resolve_cast_kind` above, not an extension of it.
    /// Two directions only: fat-to-fat is always a `Reinterpret` (every
    /// member already shares the identical `[ptr, len]` leaf shape, so
    /// there's nothing to actually convert); fat-to-thin
    /// (`*u8`/`*i8`) is `DropLength`. No reverse (thin-to-fat) --
    /// fabricating a length from a bare pointer isn't a cast, it's
    /// conjuring data that isn't there, so it's simply not offered.
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

    /// `<*[?]T>ptr` where `ptr: *[N]T`/`*mut [N]T` -- the one thin-to-fat
    /// cast this language does offer, because unlike a bare pointer, a
    /// `SizedArray`'s own type already carries its length (`N`); there's
    /// nothing to fabricate. Genuinely separate machinery from
    /// `byte_pointer_cast_kind` above (that one's gated on `is_byte_run`,
    /// `Str`/`Slice{item:U8|I8}` specifically, and this source shape never
    /// matches it) and from `resolve_cast_kind`'s scalar path below (a
    /// `Slice` target has no `cast_class` at all). Item type must match
    /// exactly -- no recursive/implicit narrowing, the same "shapes
    /// already agree, this just relabels" rule every other `CastKind`
    /// follows.
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

    /// `<*[?]T>ptr` / `<*mut T>arr` -- `Pointer` and `Array` are both exactly
    /// one `Leaf::Ptr` (`layout::leaves_of`), so converting between them is
    /// a pure `Reinterpret`, no leaf-count change at all -- the same
    /// "shapes already agree, nothing to convert" case `*str <-> *[]u8`
    /// already is.
    /// Deliberately **not** requiring the pointee/item types to match --
    /// this mirrors how an ordinary `*Foo -> *Bar` cast doesn't require
    /// `Foo == Bar` either (every `Pointer`, regardless of pointee, is the
    /// same `CastClass`): both sides here are just "a thin pointer,"
    /// reinterpreted freely, matching every existing pointer-to-pointer
    /// cast's own precedent. Either direction; the unconditional mutable-
    /// widening check earlier in `analyze_cast` (via `pointer_like_mutable`,
    /// which already covers `Array`) still applies on top of this.
    fn array_pointer_cast_kind(source: &ResolvedType, target: &ResolvedType) -> Option<CastKind> {
        match (source, target) {
            (ResolvedType::Pointer { .. }, ResolvedType::Array(_, _))
            | (ResolvedType::Array(_, _), ResolvedType::Pointer { .. }) => {
                Some(CastKind::Reinterpret)
            }
            _ => None,
        }
    }

    /// A block's own effective type: its tail expression's type, or -- if it
    /// has none -- `Void`, *unless* its last statement unconditionally
    /// diverges (see `stmt_diverges`), in which case the block itself never
    /// actually produces `Void` at its own position (control leaves the
    /// function entirely) -- so it's exempt from whatever type is expected
    /// there, the same way Rust's `!` (never) type unifies with anything.
    /// `None` here means exactly that: "diverges, no constraint," not "has
    /// no type."
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
