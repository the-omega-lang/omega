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
    pub(super) fn analyze_expr(&mut self, node: &HirExprNode, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
        let id = node.id;
        let span = node.span;
        let literal = |r#type, kind| Some(CheckedExprNode { id, span, r#type, kind });

        match &node.expr {
            HirExpr::Place(place) => self.analyze_place_read(id, span, place, expected),
            HirExpr::Hidden(inner) => self.analyze_hidden(id, span, inner, expected),
            HirExpr::Number(number) => self.analyze_number(id, span, number, expected),
            HirExpr::Bool(b) => literal(ResolvedType::Bool, CheckedExpr::Bool(*b)),
            HirExpr::Char(c) => literal(ResolvedType::Char, CheckedExpr::Char(*c)),

            // A string literal is a `*str` -- a UTF-8 byte run with a
            // compile-time-known length and no null terminator (see
            // `ResolvedType::Str`). Its bytes are raw UTF-8, not decoded
            // characters, unlike `*char`. Immutable, like every literal.
            HirExpr::String(s) => literal(ResolvedType::Str { mutable: false }, CheckedExpr::String(s.0.clone())),

            // `b"..."` -- a raw byte run with a compile-time-known length,
            // not a null-terminated C string: `*[u8]` (see `Context::
            // resolve_type`'s `*[T]` case), never `*u8`.
            HirExpr::ByteString(s) => literal(
                ResolvedType::Slice { item: Box::new(ResolvedType::U8), mutable: false },
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

            HirExpr::If(HirIf { branches, else_branch }) => {
                self.analyze_if(id, span, branches, else_branch.as_ref(), expected)
            }
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
            HirExpr::ArrayLiteral(elements) => self.analyze_array_literal(id, span, elements, expected),
            HirExpr::StructLiteral(lit) => self.analyze_struct_literal(id, span, lit),
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
        }
    }

    /// An ordinary *read* of a place. This is the only path a read takes:
    /// an assignment's own target is resolved separately (see
    /// `require_mutable_place`'s `mark_written` for the write side), while a
    /// compound assignment's or increment's synthesized read component
    /// desugars to a `HirExpr::Place` and arrives back here.
    fn analyze_place_read(&mut self, id: HirId, span: Span, place: &HirPlace, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
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
        Some(CheckedExprNode { id, span, r#type, kind: CheckedExpr::Place(checked_place) })
    }

    /// `hidden base` -- fully transparent: this produces exactly what
    /// analyzing `base` alone would (this node's own `id`/`span` are
    /// discarded in favor of `base`'s), with a `hidden_stack` frame pushed
    /// around it. See `check_visibility`/`hidden_stack`.
    fn analyze_hidden(&mut self, id: HirId, span: Span, inner: &HirExprNode, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
        self.hidden_stack.push(false);
        let result = self.analyze_expr(inner, expected);
        let load_bearing = self.hidden_stack.pop().expect("just pushed above");
        if !load_bearing {
            self.warn(id, span, AnalysisWarningKind::UnnecessaryHidden);
        }
        result
    
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
                    AnalysisErrorKind::NonBoolCondition { r#type: checked_cond.r#type },
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
                    None => {
                        Self::block_type(&checked_block).map(|t| t.widened()).unwrap_or(ResolvedType::Void)
                    }
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
        let branch_kinds: Vec<Option<ResolvedType>> =
            checked_branches.iter().map(|(_, b)| Self::block_type(b)).collect();
        let else_kind: Option<Option<ResolvedType>> = checked_else.as_ref().map(Self::block_type);

        // Widened: branches producing *different variants* of one
        // enum (`if c { E::A } else { E::B }`) still agree on the
        // enum itself, which is then the whole `if`'s type.
        let result_type = match &else_kind {
            Some(k) => branch_kinds.iter().cloned().chain(std::iter::once(k.clone())).flatten().next(),
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
            self.error(node_id, span, AnalysisErrorKind::IfBranchTypeMismatch { expected: result_type, found });
            return None;
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: result_type,
            kind: CheckedExpr::If(CheckedIf { branches: checked_branches, else_branch: checked_else }),
        })
    
    }

    /// An ordinary call, after the three interceptors (overloaded,
    /// overloaded-static, generic) have each declined it.
    fn analyze_call(&mut self, node_id: HirId, span: Span, call: &HirFunctionCall) -> Option<CheckedExprNode> {
        // Tried in priority order; the first to claim the call answers it.
        let interceptors: [Interceptor<'r>; 3] = [
            Self::resolve_overloaded_call,
            Self::resolve_overloaded_static_call,
            Self::resolve_generic_call,
        ];
        for intercept in interceptors {
            if let Intercepted::Claimed(result) = intercept(self, node_id, span, call) {
                return result;
            }
        }

        let ResolvedCallee { callee, fn_type, implicit_self, checked_args } =
            match self.resolve_callee(&call.callee, &call.args)? {
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

                    let expected_type =
                        (param_index < fn_type.params.len()).then(|| &fn_type.params[param_index].1);
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
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee), fn_type, args }),
        })
    
    }

    /// `target = value`.
    fn analyze_assignment(&mut self, node_id: HirId, span: Span, assignment: &omega_hir::HirAssignment) -> Option<CheckedExprNode> {
        let (was_hidden, target) = Self::strip_hidden(&assignment.target);
        let HirExpr::Place(place) = &target.expr else {
            self.error(node_id, span, AnalysisErrorKind::AssignmentTargetNotAPlace);
            return None;
        };
        // `was_hidden` activates the bypass for `analyze_place`'s own
        // field-visibility checks -- see `strip_hidden`'s doc
        // comment for why `hidden` on an assignment's target (`hidden
        // a.b = c;`) never reaches `analyze_expr`'s own `HirExpr::
        // Hidden` arm at all (it wraps only `target`, never the whole
        // `Assignment`).
        let (checked_target, target_type, target_mutable) = self.with_hidden_bypass(
            was_hidden,
            node_id,
            span,
            |this| this.analyze_place(target.id, target.span, place, None),
        )?;
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
        let (was_hidden, base) = Self::strip_hidden(base);
        // `&base[range]`/`&mut base[range]` -- a slice, not an ordinary
        // pointer; see `analyze_slice` for why this is the *only* way to
        // produce one. Both this and the compile-time-slice form below run
        // under `was_hidden` exactly like the plain-place form further down:
        // a stripped `hidden` has to keep its bypass at *every* operand
        // position, not just the one that happens to reach `analyze_place`
        // directly.
        if let HirExpr::Slice(HirSlice { base: slice_base, range }) = &base.expr {
            return self.with_hidden_bypass(was_hidden, node_id, span, |this| {
                this.analyze_slice(node_id, span, slice_base, range, mutable)
            });
        }
        // `&[...]`/`&mut [...]` -- a compile-time slice, not an ordinary
        // place; see `analyze_const_slice`.
        if let HirExpr::ArrayLiteral(elements) = &base.expr {
            return self.with_hidden_bypass(was_hidden, node_id, span, |this| {
                this.analyze_const_slice(node_id, span, elements, mutable, expected)
            });
        }
        let HirExpr::Place(place) = &base.expr else {
            self.error(node_id, span, AnalysisErrorKind::AddressOfNotAPlace);
            return None;
        };
        // See `Analyzer::strip_hidden`'s doc comment -- same
        // reasoning as `HirExpr::Assignment`'s arm.
        let (checked_place, place_type, place_mutable) = self.with_hidden_bypass(
            was_hidden,
            node_id,
            span,
            |this| this.analyze_place(base.id, base.span, place, None),
        )?;

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
            let narrowed_shadow = self
                .narrowable_place(place)
                .is_some_and(|(ident, ..)| self.context.find_variable(&ident).is_some_and(|b| b.narrowed));
            if narrowed_shadow { place_type.widened() } else { place_type }
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Pointer { pointee: Box::new(pointee_type), mutable },
            kind: CheckedExpr::AddressOf(CheckedAddressOf { place: checked_place }),
        })
    
    }

    /// Unary `-`. `expected` passes straight through: negation is
    /// transparent to its own result type, so this node's own type context
    /// is exactly right for `base` too (notably, `-100` is exactly as
    /// adaptable as `100`).
    fn analyze_negate(&mut self, node_id: HirId, span: Span, base: &HirExprNode, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
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
            self.error(node_id, span, AnalysisErrorKind::InvalidNegateOperand { r#type: checked_base.r#type });
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
    /// `analyze_negate`.
    fn analyze_bit_not(&mut self, node_id: HirId, span: Span, base: &HirExprNode, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
        // `expected` passes straight through, same reasoning as
        // `Negate`'s arm just above -- `~` is transparent to its own
        // result type.
        let checked_base = self.analyze_expr(base, expected)?;
        let bitnotable =
            matches!(checked_base.r#type.numeric_kind(), Some(NumericKind::Signed(_) | NumericKind::Unsigned(_)));
        if !bitnotable {
            self.error(node_id, span, AnalysisErrorKind::InvalidBitNotOperand { r#type: checked_base.r#type });
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
    fn analyze_binary_expr(&mut self, node_id: HirId, span: Span, bin: &omega_hir::HirBinaryOp, expected: Option<&ResolvedType>) -> Option<CheckedExprNode> {
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
        let operand_expected = if bin.op.is_comparison() { None } else { expected };
        let checked_left = self.analyze_expr(&bin.left, operand_expected)?;
        let left_type = checked_left.r#type.widened();
        let checked_right = self.analyze_expr(&bin.right, operand_expected.or(Some(&left_type)))?;
        self.analyze_binary_op(node_id, span, bin.op, checked_left, checked_right)
    
    }

    /// `<Target>base`.
    fn analyze_cast(&mut self, node_id: HirId, span: Span, target: &Type, base: &HirExprNode) -> Option<CheckedExprNode> {
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

        // The str/byte-slice family (`*str`/`*[u8]`/`*[i8]`) is
        // tried first: a fat pointer doesn't fit `cast_class`'s
        // scalar-width model at all (`Str`/`Slice` both return
        // `None` from it), so this is genuinely separate machinery,
        // not an extension of the numeric path below.
        let cast_kind = if let Some(kind) = Self::byte_pointer_cast_kind(&checked_base.r#type, &target_type) {
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
            Self::resolve_cast_kind(source_class, target_class)
        };
        if cast_kind == CastKind::Reinterpret && checked_base.r#type == target_type {
            self.warn(node_id, span, AnalysisWarningKind::NoOpCast { r#type: target_type.clone() });
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
    fn analyze_incr_decr(&mut self, node_id: HirId, span: Span, base: &HirExprNode, op: BinaryOp) -> Option<CheckedExprNode> {
        let (was_hidden, base) = Self::strip_hidden(base);
        let HirExpr::Place(place) = &base.expr else {
            self.error(node_id, span, AnalysisErrorKind::IncrementTargetNotAPlace);
            return None;
        };
        // See `Analyzer::strip_hidden`'s doc comment -- same reasoning as
        // `HirExpr::Assignment`'s arm.
        let (checked_place, place_type, mutable) =
            self.with_hidden_bypass(was_hidden, node_id, span, |this| this.analyze_place(base.id, base.span, place, None))?;
        self.require_mutable_place(node_id, span, &place.root, &checked_place, mutable)?;

        let Some(kind) = place_type.numeric_kind() else {
            self.error(node_id, span, AnalysisErrorKind::InvalidIncrementOperand { r#type: place_type });
            return None;
        };

        let one = match kind {
            NumericKind::Signed(_) => NumberValue::Signed(1),
            NumericKind::Unsigned(_) => NumberValue::Unsigned(1),
            NumericKind::Float(_) => NumberValue::Float(1.0),
        };
        let one_node = CheckedExprNode { id: node_id, span, r#type: place_type.clone(), kind: CheckedExpr::Number(one) };
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
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(place_read), right: Box::new(one_node) }),
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type,
            kind: CheckedExpr::Assignment(CheckedAssignment { target: checked_place, value: Box::new(sum) }),
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
        // `char` is comparable (it's `numeric_kind`-shaped underneath --
        // one unsigned 4-byte scalar value, ordered by codepoint), but
        // never arithmetic/bitwise: combining two `char`s that way can
        // produce a codepoint that isn't a valid Unicode scalar value at
        // all, and this language has no fallible/validating path for that
        // yet (see `ResolvedType::Char`'s doc comment). So `char` is
        // accepted here *only* for a comparison op -- everything else
        // still requires genuine `numeric_kind`.
        for operand in [&checked_left, &checked_right] {
            let is_valid =
                operand.r#type.numeric_kind().is_some() || (op.is_comparison() && operand.r#type == ResolvedType::Char);
            if !is_valid {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::InvalidBinaryOperand { op, r#type: operand.r#type.clone() },
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
        if op == BinaryOp::Rem && matches!(checked_left.r#type.numeric_kind(), Some(NumericKind::Float(_))) {
            self.error(node_id, span, AnalysisErrorKind::FloatRemainder);
            return None;
        }

        // No native float bitwise/shift instructions either -- same
        // reasoning as `Rem` just above, just for a whole family of ops
        // instead of one.
        if matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr)
            && matches!(checked_left.r#type.numeric_kind(), Some(NumericKind::Float(_)))
        {
            self.error(node_id, span, AnalysisErrorKind::FloatBitwiseOperand);
            return None;
        }

        if op.is_comparison() {
            self.check_always_true_false_comparison(node_id, span, op, &checked_left, &checked_right);
        }

        // A comparison always produces `bool`, regardless of the
        // (still-numeric, still-matching) operand type; an
        // arithmetic op's result is that same operand type.
        let r#type = if op.is_comparison() { ResolvedType::Bool } else { checked_left.r#type.clone() };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(checked_left), right: Box::new(checked_right) }),
        })
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
        let Some((lo, hi)) = left.r#type.integer_domain() else { return };

        let (literal, literal_on_right) = match (Self::literal_i128(left), Self::literal_i128(right)) {
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
                BinaryOp::Lt => (hi < literal).then_some(true).or((lo >= literal).then_some(false)),
                BinaryOp::Le => (hi <= literal).then_some(true).or((lo > literal).then_some(false)),
                BinaryOp::Gt => (lo > literal).then_some(true).or((hi <= literal).then_some(false)),
                BinaryOp::Ge => (lo >= literal).then_some(true).or((hi < literal).then_some(false)),
                BinaryOp::Eq => (literal < lo || literal > hi).then_some(false),
                BinaryOp::Ne => (literal < lo || literal > hi).then_some(true),
                _ => None,
            }
        } else {
            match op {
                BinaryOp::Lt => (literal < lo).then_some(true).or((literal >= hi).then_some(false)),
                BinaryOp::Le => (literal <= lo).then_some(true).or((literal > hi).then_some(false)),
                BinaryOp::Gt => (literal > hi).then_some(true).or((literal <= lo).then_some(false)),
                BinaryOp::Ge => (literal >= hi).then_some(true).or((literal < lo).then_some(false)),
                BinaryOp::Eq => (literal < lo || literal > hi).then_some(false),
                BinaryOp::Ne => (literal < lo || literal > hi).then_some(true),
                _ => None,
            }
        };

        if let Some(result) = fixed {
            self.warn(node_id, span, AnalysisWarningKind::AlwaysTrueFalseComparison { result });
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
        let (was_hidden, target) = Self::strip_hidden(target);
        let HirExpr::Place(place) = &target.expr else {
            self.error(node_id, span, AnalysisErrorKind::CompoundAssignTargetNotAPlace);
            return None;
        };
        // See `Analyzer::strip_hidden`'s doc comment -- same reasoning as
        // `HirExpr::Assignment`'s arm.
        let (checked_place, place_type, mutable) = self.with_hidden_bypass(
            was_hidden,
            node_id,
            span,
            |this| this.analyze_place(target.id, target.span, place, None),
        )?;
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
            kind: CheckedExpr::Assignment(CheckedAssignment { target: checked_place, value: Box::new(combined) }),
        })
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
            (CastClass::Int { signed, .. }, CastClass::Float { .. }) => CastKind::IntToFloat { signed },
            (CastClass::Float { .. }, CastClass::Int { signed, .. }) => CastKind::FloatToInt { signed },
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
