use super::*;

impl<'r> Analyzer<'r> {
    /// `match scrutinee { pattern => body, ... } else { ... }` -- both an
    /// exhaustive switch and the proof mechanism behind sum-type subtyping.
    /// For an enum scrutinee that is exactly a bare local/parameter
    /// reference (`narrowable_scrutinee`), each arm that names a specific
    /// variant re-declares that same binding, narrowed to that variant, for
    /// the duration of analyzing the arm's body -- ordinary lexical
    /// shadowing (`Context::enter_scope`/`declare_binding`), not a new
    /// mechanism; `Context::find_variable` already prefers the innermost
    /// scope, and shadowing an outer binding is already-supported ordinary
    /// scoping. `else` (when present) always analyzes against the
    /// un-narrowed type, matching the "still just a generic Entity" rule.
    ///
    /// Any other scrutinee shape (a field access, a deref, a plain
    /// non-place expression, ...) is still fully supported for branching,
    /// just without narrowing -- there's no name to narrow -- and is
    /// evaluated exactly once into a synthesized local first, so a
    /// side-effecting scrutinee expression (e.g. a function call) isn't
    /// silently re-run once per arm the way re-parsing the source
    /// expression per condition would.
    pub(super) fn analyze_match(&mut self, node_id: HirId, span: Span, m: &HirMatch) -> Option<CheckedExprNode> {
        let narrow_target = self.narrowable_scrutinee(&m.scrutinee);
        let checked_scrutinee = self.analyze_expr(&m.scrutinee, None)?;
        let scrutinee_type = checked_scrutinee.r#type.clone();

        let (scrutinee_read, prelude_stmts, narrow_binding) = if let Some((ident, decl_id, storage, mutable)) = narrow_target {
            (checked_scrutinee.clone(), Vec::new(), Some((ident, decl_id, storage, mutable)))
        } else {
            let target = CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id: node_id, storage: Storage::Local, r#type: scrutinee_type.clone() },
                projections: vec![],
            };
            let decl = CheckedStmt::Declaration(CheckedDeclaration {
                id: node_id,
                span,
                ident: Ident("$scrutinee".to_string()),
                r#type: scrutinee_type.clone(),
            });
            let assign = CheckedStmt::Expression(CheckedExprNode {
                id: node_id,
                span,
                r#type: scrutinee_type.clone(),
                kind: CheckedExpr::Assignment(CheckedAssignment { target: target.clone(), value: Box::new(checked_scrutinee) }),
            });
            let read = CheckedExprNode { id: node_id, span, r#type: scrutinee_type.clone(), kind: CheckedExpr::Place(target) };
            (read, vec![decl, assign], None)
        };

        let is_enum_scrutinee = matches!(&scrutinee_type, ResolvedType::Enum { .. })
            || matches!(&scrutinee_type, ResolvedType::Pointer { pointee, .. } if matches!(**pointee, ResolvedType::Enum { .. }));

        let (arms, else_branch, result_type) = if is_enum_scrutinee {
            self.analyze_enum_match(node_id, span, m, &scrutinee_type, &scrutinee_read, narrow_binding)?
        } else if scrutinee_type.integer_domain().is_some() {
            self.analyze_value_match(node_id, span, m, &scrutinee_type, &scrutinee_read)?
        } else {
            self.error(node_id, span, AnalysisErrorKind::UnsupportedMatchScrutinee { r#type: scrutinee_type });
            return None;
        };

        let checked_match = CheckedExprNode {
            id: node_id,
            span,
            r#type: result_type.clone(),
            kind: CheckedExpr::Match(CheckedMatch { arms, else_branch }),
        };

        if prelude_stmts.is_empty() {
            Some(checked_match)
        } else {
            Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: result_type,
                kind: CheckedExpr::Codeblock(CheckedBlock { stmts: prelude_stmts, tail: Some(Box::new(checked_match)) }),
            })
        }
    }

    /// A place that's exactly a bare local/parameter reference -- no
    /// projections, no module qualification, no explicit generic
    /// arguments. Only this shape supports narrowing/de-assumption (see
    /// `analyze_match`'s and `HirExpr::AddressOf`'s doc comments): its
    /// identity (`decl_id`/`storage`) is what gets shadow-declared (match
    /// narrowing) or widened in place (`Context::widen_variable`, `&mut`'s
    /// de-assumption).
    pub(super) fn narrowable_place(&self, place: &HirPlace) -> Option<(Ident, HirId, Storage, bool)> {
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else { return None };
        if !expr_path.generic_args.is_empty() || !expr_path.path.is_unqualified() {
            return None;
        }
        let binding = self.context.find_variable(&expr_path.path.head)?;
        Some((expr_path.path.head.clone(), binding.decl_id, binding.storage, binding.mutable))
    }

    /// `narrowable_place`, but for a scrutinee expression rather than an
    /// already-unwrapped place -- `match`'s scrutinee is a full
    /// `HirExprNode` (it doesn't have to be a place at all, see
    /// `analyze_match`'s doc comment), so this just unwraps the one shape
    /// that's ever narrowable before delegating.
    fn narrowable_scrutinee(&self, scrutinee: &HirExprNode) -> Option<(Ident, HirId, Storage, bool)> {
        let HirExpr::Place(place) = &Self::strip_reveal(scrutinee).1.expr else { return None };
        self.narrowable_place(place)
    }

    /// An arm's body: a `{ ... }` block analyzes exactly like any other
    /// codeblock; a bare expression (`100 => "a hundred"`, no braces) is
    /// wrapped in a trivial one-tail-expression block, so codegen only ever
    /// deals with one shape (the same normalization `if`'s branches don't
    /// need, since `if` never allows a bare-expression branch body).
    fn analyze_match_arm_body(&mut self, body: &HirExprNode) -> Option<CheckedBlock> {
        if let HirExpr::Codeblock(block) = &body.expr {
            self.analyze_block(block, None)
        } else {
            let checked = self.analyze_expr(body, None)?;
            Some(CheckedBlock { stmts: vec![], tail: Some(Box::new(checked)) })
        }
    }

    /// The `analyze_match` case where `scrutinee_type` is `Enum{..}` or
    /// `Pointer(Enum{..})` -- through a pointer, a matched arm narrows the
    /// pointer's own pointee refinement (`*Entity` -> `*Entity::Person`),
    /// exactly like the plain-value case narrows the value's own type
    /// (see `analyze_match`'s doc comment).
    fn analyze_enum_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_read: &CheckedExprNode,
        narrow_binding: Option<(Ident, HirId, Storage, bool)>,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        // `through_pointer` is the scrutinee's own pointer mutability when
        // matching through one -- narrowing only ever refines the pointee,
        // never changes whether the pointer itself is writable.
        let (cell, through_pointer) = match scrutinee_type {
            ResolvedType::Enum { cell, .. } => (cell.clone(), None),
            ResolvedType::Pointer { pointee, mutable } => match &**pointee {
                ResolvedType::Enum { cell, .. } => (cell.clone(), Some(*mutable)),
                _ => unreachable!("caller already confirmed this is an enum or pointer-to-enum"),
            },
            _ => unreachable!("caller already confirmed this is an enum or pointer-to-enum"),
        };

        // `.tag` -- the same read regardless of which variant an arm
        // matches (every value of an enum carries one); built once here,
        // reused (cloned) per arm's condition.
        let mut tag_projections = Vec::new();
        let tag_type = self.resolve_field_projection(
            node_id,
            span,
            &mut tag_projections,
            scrutinee_type,
            &Ident("tag".to_string()),
            &mut false,
        )?;
        let CheckedExpr::Place(scrutinee_place) = &scrutinee_read.kind else {
            unreachable!("analyze_match always builds scrutinee_read as a place read")
        };
        let tag_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: tag_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace { root: scrutinee_place.root.clone(), projections: tag_projections }),
        };

        let mut covered: HashMap<usize, Span> = HashMap::new();
        let mut checked_arms = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            let HirPattern::Value(pattern_expr) = &arm.pattern else {
                self.error(
                    node_id,
                    arm.pattern.span(),
                    AnalysisErrorKind::PatternNotEnumVariant { r#enum: cell.borrow().name.clone() },
                );
                return None;
            };
            let variant_index = self.resolve_variant_pattern(&cell, pattern_expr)?;

            if let Some(previous) = covered.insert(variant_index, arm.pattern.span()) {
                self.error(node_id, arm.pattern.span(), AnalysisErrorKind::OverlappingMatchArm { previous });
                return None;
            }

            let tag_const = CheckedExprNode {
                id: node_id,
                span: arm.pattern.span(),
                r#type: tag_type.clone(),
                kind: CheckedExpr::Number(cell.borrow().variants[variant_index].tag),
            };
            let condition = CheckedExprNode {
                id: node_id,
                span: arm.pattern.span(),
                r#type: ResolvedType::Bool,
                kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                    op: BinaryOp::Eq,
                    left: Box::new(tag_read.clone()),
                    right: Box::new(tag_const),
                }),
            };

            self.context.enter_scope();
            if let Some((ident, decl_id, storage, mutable)) = &narrow_binding {
                let refined = ResolvedType::Enum { cell: cell.clone(), variant: Some(variant_index) };
                let narrowed = match through_pointer {
                    Some(pointer_mutable) => ResolvedType::Pointer { pointee: Box::new(refined), mutable: pointer_mutable },
                    None => refined,
                };
                self.declare_narrowed_binding(*decl_id, arm.span, ident, narrowed, *storage, *mutable);
            }
            let body = self.analyze_match_arm_body(&arm.body);
            self.context.leave_scope();

            checked_arms.push(CheckedMatchArm { conditions: vec![condition], body: body? });
        }

        let variant_count = cell.borrow().variants.len();
        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if covered.len() < variant_count => {
                let missing: Vec<Ident> = cell
                    .borrow()
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| !covered.contains_key(idx))
                    .map(|(_, v)| v.name.clone())
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchEnum { r#enum: cell.borrow().name.clone(), missing },
                );
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    /// Resolves a match pattern that must name one of `cell`'s variants
    /// (`Entity::Person`) -- deliberately its own, simpler lookup rather
    /// than reusing `resolve_type_member`/`resolve_unit_variant`: those
    /// build a *construction* (rejecting a variant that has body fields,
    /// via `EnumVariantMissingBody`), which is wrong here -- a pattern only
    /// ever tests the tag, so a variant with a body is just as matchable as
    /// one without.
    fn resolve_variant_pattern(&mut self, cell: &Rc<RefCell<ResolvedEnumType>>, expr: &HirExprNode) -> Option<usize> {
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
                AnalysisErrorKind::PatternNotEnumVariant { r#enum: cell.borrow().name.clone() },
            );
            return None;
        }
        let HirExpr::Place(HirPlace { root: HirPlaceRoot::Path(expr_path), .. }) = &expr.expr else {
            unreachable!("just confirmed above")
        };
        let variant_name = expr_path.path.tail.last().expect("just confirmed non-empty above");
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
                    scrutinee: ResolvedType::Enum { cell: cell.clone(), variant: None },
                },
            );
            return None;
        }
        let found = cell.borrow().variant(variant_name).map(|(idx, _)| idx);
        match found {
            Some(idx) => Some(idx),
            None => {
                let similar = best_match(variant_name, cell.borrow().variants.iter().map(|v| &v.name));
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

    /// The `analyze_match` case where `scrutinee_type` is an integer type or
    /// `Bool` (`ResolvedType::integer_domain`'s `Some` cases) -- value and
    /// range patterns, checked for full-domain coverage by
    /// `crate::exhaustiveness`.
    fn analyze_value_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_read: &CheckedExprNode,
    ) -> Option<(Vec<CheckedMatchArm>, Option<CheckedBlock>, ResolvedType)> {
        let domain = scrutinee_type
            .integer_domain()
            .expect("caller already confirmed this type has an integer domain");

        let mut checked_arms = Vec::with_capacity(m.arms.len());
        let mut intervals = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            let (lo, hi, conditions) = self.analyze_value_pattern(&arm.pattern, scrutinee_type, scrutinee_read)?;
            intervals.push(crate::exhaustiveness::Interval { lo, hi, span: arm.pattern.span() });
            let body = self.analyze_match_arm_body(&arm.body)?;
            checked_arms.push(CheckedMatchArm { conditions, body });
        }

        let coverage = crate::exhaustiveness::check(domain, intervals);
        if !coverage.overlaps.is_empty() {
            for (a, b) in &coverage.overlaps {
                // The sweep finds the pair in interval order, which is not
                // the order they were written: a catch-all written *last*
                // still sorts first, and blaming the arms above it for
                // overlapping something further down reads backwards. The
                // arm written later is always the one to point at.
                let (earlier, later) =
                    if a.span.start <= b.span.start { (a.span, b.span) } else { (b.span, a.span) };
                self.error(node_id, later, AnalysisErrorKind::OverlappingMatchArm { previous: earlier });
            }
            return None;
        }

        let else_branch = match &m.else_branch {
            Some(b) => Some(self.analyze_block(b, None)?),
            None if !coverage.gaps.is_empty() => {
                let gaps = coverage.gaps.iter().map(|(lo, hi)| Self::describe_gap(scrutinee_type, *lo, *hi)).collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NonExhaustiveMatchValue { r#type: scrutinee_type.clone(), gaps },
                );
                return None;
            }
            None => None,
        };

        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some((checked_arms, else_branch, result_type))
    }

    /// One value/range pattern's covered interval, plus the runtime
    /// condition(s) that test it -- one condition per bound actually
    /// present (a range pattern's absent bound needs no runtime check at
    /// all, since it's already the domain's own edge).
    fn analyze_value_pattern(
        &mut self,
        pattern: &HirPattern,
        scrutinee_type: &ResolvedType,
        scrutinee_read: &CheckedExprNode,
    ) -> Option<(i128, i128, Vec<CheckedExprNode>)> {
        match pattern {
            HirPattern::Value(expr) => {
                let value = self.const_eval_pattern(expr, scrutinee_type)?;
                let n = Self::const_value_as_i128(&value);
                let condition = Self::value_cmp_condition(scrutinee_read, expr.id, expr.span, scrutinee_type, BinaryOp::Eq, value);
                Some((n, n, vec![condition]))
            }
            HirPattern::Range(range) => {
                let domain = scrutinee_type.integer_domain().expect("caller already confirmed an integer domain");
                let mut conditions = Vec::new();
                let lo = match &range.start {
                    Some(e) => {
                        let value = self.const_eval_pattern(e, scrutinee_type)?;
                        let n = Self::const_value_as_i128(&value);
                        conditions.push(Self::value_cmp_condition(scrutinee_read, e.id, e.span, scrutinee_type, BinaryOp::Ge, value));
                        n
                    }
                    None => domain.0,
                };
                let hi = match &range.end {
                    Some(e) => {
                        let value = self.const_eval_pattern(e, scrutinee_type)?;
                        let n = Self::const_value_as_i128(&value);
                        let op = if range.inclusive { BinaryOp::Le } else { BinaryOp::Lt };
                        conditions.push(Self::value_cmp_condition(scrutinee_read, e.id, e.span, scrutinee_type, op, value));
                        if range.inclusive { n } else { n - 1 }
                    }
                    None => domain.1,
                };
                Some((lo, hi, conditions))
            }
        }
    }

    /// The pattern-position sibling of `const_eval`: a literal constant
    /// (a number, optionally negated, a bool, or a char), checked against
    /// `expected` -- the scrutinee's own exact type drives interpretation,
    /// same reasoning as `const_eval`. Deliberately its own function rather
    /// than reusing `const_eval` outright: that function's fallback error
    /// (`EnumValueNotConstant`) is worded specifically for enum header
    /// values, which would be a confusing thing to say about a match
    /// pattern; `const_number` itself (the actual parsing/range-checking
    /// logic) is fully shared.
    fn const_eval_pattern(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number),
                _ => {
                    self.error(expr.id, expr.span, AnalysisErrorKind::PatternValueNotConstant);
                    None
                }
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => {
                    self.error(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::PatternTypeMismatch { expected: expected.clone(), found: ResolvedType::Char },
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
                        AnalysisErrorKind::PatternTypeMismatch { expected: expected.clone(), found: ResolvedType::Bool },
                    );
                    None
                }
            },
            _ => {
                self.error(expr.id, expr.span, AnalysisErrorKind::PatternValueNotConstant);
                None
            }
        }
    }

    fn const_value_as_i128(value: &ConstValue) -> i128 {
        match value {
            ConstValue::Number(NumberValue::Signed(n)) => *n as i128,
            ConstValue::Number(NumberValue::Unsigned(n)) => *n as i128,
            ConstValue::Number(NumberValue::Float(_)) => {
                unreachable!("match patterns are never float-typed -- integer_domain excludes floats")
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
                unreachable!("analyze_value_match only ever runs for an integer/bool/char scrutinee type")
            }
        }
    }

    fn value_cmp_condition(
        scrutinee_read: &CheckedExprNode,
        id: HirId,
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
                unreachable!("analyze_value_match only ever runs for an integer/bool/char scrutinee type")
            }
        };
        let constant = CheckedExprNode { id, span, r#type: scrutinee_type.clone(), kind };
        CheckedExprNode {
            id,
            span,
            r#type: ResolvedType::Bool,
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(scrutinee_read.clone()), right: Box::new(constant) }),
        }
    }

    /// A gap's inclusive `[lo, hi]` bounds, formatted for
    /// `NonExhaustiveMatchValue`'s diagnostic -- `bool`'s domain renders as
    /// `true`/`false` rather than `0`/`1`; `char`'s renders as an actual
    /// quoted character for the ordinary printable ASCII range (covering
    /// the common case -- letter/digit ranges like `'A'...'Z'`, exactly
    /// how such a pattern is actually written), and falls back to a
    /// `U+XXXX` codepoint label for everything else, rather than risk
    /// printing an invisible, misleading, or malformed glyph into a
    /// terminal (whitespace/control codes, non-ASCII, and the small
    /// unassigned surrogate hole `integer_domain`'s doc comment mentions).
    fn describe_gap(scrutinee_type: &ResolvedType, lo: i128, hi: i128) -> String {
        let render = |n: i128| match scrutinee_type {
            ResolvedType::Bool => if n == 0 { "false".to_string() } else { "true".to_string() },
            ResolvedType::Char => match char::from_u32(n as u32) {
                Some(c) if c.is_ascii_graphic() || c == ' ' => format!("'{c}'"),
                _ => format!("U+{n:04X}"),
            },
            _ => n.to_string(),
        };
        if lo == hi { render(lo) } else { format!("{}..={}", render(lo), render(hi)) }
    }

    /// Unifies every arm's (and `else`'s, if present) resolved type exactly
    /// like `HirExpr::If` does (see that arm's own comments) -- first
    /// concrete (non-diverging) type, widened, checked against every other
    /// arm via `accepts`. Unlike `if`, an absent `else` never forces `Void`:
    /// by the time this runs, `analyze_enum_match`/`analyze_value_match`
    /// have already guaranteed the arms are exhaustive on their own, so a
    /// real value always comes from some arm.
    fn unify_match_arm_types(
        &mut self,
        node_id: HirId,
        span: Span,
        arms: &[CheckedMatchArm],
        else_branch: &Option<CheckedBlock>,
    ) -> Option<ResolvedType> {
        let arm_kinds: Vec<Option<ResolvedType>> = arms.iter().map(|a| Self::block_type(&a.body)).collect();
        let else_kind: Option<Option<ResolvedType>> = else_branch.as_ref().map(Self::block_type);

        let result_type = arm_kinds
            .iter()
            .cloned()
            .chain(else_kind.iter().cloned())
            .flatten()
            .next()
            .map(|t| t.widened())
            .unwrap_or(ResolvedType::Void);

        let mismatch = arm_kinds.into_iter().chain(else_kind).flatten().find(|t| !result_type.accepts(t));
        if let Some(found) = mismatch {
            self.error(node_id, span, AnalysisErrorKind::MatchArmTypeMismatch { expected: result_type, found });
            return None;
        }
        Some(result_type)
    }
}
