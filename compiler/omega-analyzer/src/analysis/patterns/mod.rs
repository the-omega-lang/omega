use super::*;
use crate::exhaustiveness::{self as space, Pat, Relation};
use resolve::{Position, Proof};

mod resolve;

struct ResolvedArm<'a> {
    arm: &'a HirMatchArm,
    space: space::Arm,
    proof: Proof,
    catch_all: bool,
}

/// One analyzed `match`, before it becomes an expression node.
struct MatchShape {
    arms: Vec<CheckedMatchArm>,
    else_branch: Option<CheckedBlock>,
    remainder: CheckedMatchRemainder,
    result_type: ResolvedType,
}

impl<'r> Analyzer<'r> {
    /// `expected` is the surrounding expected type of the whole `match`, and
    /// it reaches every arm body, catch-all, and `else` exactly as it reaches
    /// an `if` branch: arms are *checked* against a type that already exists,
    /// never joined into a new one.
    pub(super) fn analyze_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        expected: Expected<'_>,
    ) -> Option<CheckedExprNode> {
        let narrow_target = self.narrowable_scrutinee(&m.scrutinee);
        let checked_scrutinee = self.analyze_expr(&m.scrutinee, Expected::None)?;
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
                    symbol: crate::annotations::SymbolPolicy::ordinary(),
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
        let supported = match matched {
            ResolvedType::Enum { .. }
            | ResolvedType::AnonymousEnum { .. }
            | ResolvedType::Struct(_) => true,
            _ => scrutinee_type
                .integer_domain(self.target.pointer_bits())
                .is_some(),
        };
        if !supported {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::UnsupportedMatchScrutinee {
                    r#type: scrutinee_type,
                },
            );
            return None;
        }
        let shape = self.analyze_pattern_match(
            node_id,
            span,
            m,
            &scrutinee_type,
            &scrutinee_place,
            narrow_binding,
            expected,
        )?;

        let MatchShape {
            arms,
            else_branch,
            remainder,
            result_type,
        } = shape;
        let checked_match = CheckedMatch {
            arms,
            else_branch,
            remainder,
        };

        if prelude_stmts.is_empty() {
            Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: result_type,
                kind: CheckedExpr::Match(checked_match),
            })
        } else {
            let checked_match = CheckedExprNode {
                id: self.resolver.fresh_synthetic_id(),
                span,
                r#type: result_type.clone(),
                kind: CheckedExpr::Match(checked_match),
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

    fn analyze_match_arm_body(
        &mut self,
        body: &HirExprNode,
        expected: Expected<'_>,
    ) -> Option<CheckedBlock> {
        if let HirExpr::Codeblock(block) = &body.expr {
            self.analyze_block(block, expected)
        } else {
            let checked = self.analyze_expr(body, expected)?;
            let checked = self.coerce_to_expected(expected.exact(), checked);
            Some(CheckedBlock {
                stmts: vec![],
                tail: Some(Box::new(checked)),
            })
        }
    }

    /// Every `match` runs through one flow: each arm is resolved into the
    /// pattern space, source order is checked against specificity, and the
    /// arms are then walked in order, each taking its part of what is left.
    /// Because order and specificity always agree, the first matching arm is
    /// the one that runs.
    fn analyze_pattern_match(
        &mut self,
        node_id: HirId,
        span: Span,
        m: &HirMatch,
        scrutinee_type: &ResolvedType,
        scrutinee_place: &CheckedPlace,
        narrow_binding: Option<(Ident, Origin, HirId, Storage, bool)>,
        expected: Expected<'_>,
    ) -> Option<MatchShape> {
        let through_pointer = match scrutinee_type {
            ResolvedType::Pointer { mutable, .. } => Some(*mutable),
            _ => None,
        };
        let matched = Self::matched_through_pointer(scrutinee_type).widened();
        let mut root_place = scrutinee_place.clone();
        if through_pointer.is_some() {
            root_place.projections.push(CheckedProjection::Deref {
                r#type: matched.clone(),
            });
            root_place.r#type = matched.clone();
        }
        let root = Position {
            place: root_place,
            r#type: matched.clone(),
            path: Vec::new(),
        };
        let tagged_root = matches!(
            matched,
            ResolvedType::Enum { .. } | ResolvedType::AnonymousEnum { .. }
        );

        let mut catch_all: Option<&HirMatchArm> = None;
        for arm in &m.arms {
            if arm.pattern.catch_all_range().is_none() {
                continue;
            }
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

        let mut arms = Vec::with_capacity(m.arms.len());
        for arm in &m.arms {
            let is_catch_all = arm.pattern.catch_all_range().is_some();
            let mut proof = Proof::default();
            let pat = if is_catch_all {
                Pat::Any
            } else {
                self.resolve_subpattern(node_id, &root, &arm.pattern, &mut proof)?
            };
            arms.push(ResolvedArm {
                arm,
                space: space::Arm {
                    alternatives: vec![pat],
                },
                proof,
                catch_all: is_catch_all,
            });
        }

        if !self.check_arm_order(node_id, &arms) {
            return None;
        }

        let mut remaining = vec![Pat::Any];
        let mut catch_all_alternatives = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            if !space::arm_intersects(&remaining, &arm.space) {
                let kind = if arm.catch_all {
                    AnalysisErrorKind::CatchAllPatternRedundant
                } else {
                    AnalysisErrorKind::UnreachableMatchArm {
                        covering: arms[..index]
                            .iter()
                            .filter(|earlier| {
                                space::relate_arms(&earlier.space, &arm.space) != Relation::Disjoint
                            })
                            .map(|earlier| earlier.arm.pattern.span())
                            .collect(),
                    }
                };
                self.error(node_id, arm.arm.pattern.span(), kind);
                return None;
            }
            if arm.catch_all {
                catch_all_alternatives = self.root_alternatives(&remaining, &matched);
            }
            remaining = space::subtract(remaining, &arm.space);
        }

        let tests_tag = arms.iter().any(|arm| arm.proof.tests_tag);
        let mut checked_arms = Vec::with_capacity(arms.len() + 1);
        for arm in arms {
            // A top-level `..` on an enum still tests the scrutinee's tag, so
            // an invalid tag reaches the remainder instead of this body.
            let conditions = match (arm.catch_all, tagged_root) {
                (true, true) => self.root_tag_groups(
                    node_id,
                    &root,
                    &catch_all_alternatives,
                    arm.arm.pattern.span(),
                )?,
                (true, false) => vec![Vec::new()],
                (false, _) => vec![arm.proof.conditions],
            };
            let refinements = arm.proof.refinements;
            let (body, _) = self.with_scope(|this| {
                if !arm.catch_all
                    && let Some(binding) = &narrow_binding
                {
                    this.narrow_scrutinee_binding(
                        binding,
                        scrutinee_type,
                        through_pointer,
                        &refinements,
                        arm.arm.span,
                    );
                }
                this.analyze_match_arm_body(&arm.arm.body, expected)
            });
            checked_arms.push(CheckedMatchArm {
                conditions,
                body: body?,
            });
        }

        if tagged_root {
            match &m.else_branch {
                Some(block) => {
                    let uncovered = self.root_alternatives(&remaining, &matched);
                    let conditions =
                        self.root_tag_groups(node_id, &root, &uncovered, block.span)?;
                    if let Some(body) =
                        self.analyze_else_arm(node_id, block, expected, !conditions.is_empty())
                    {
                        checked_arms.push(CheckedMatchArm { conditions, body });
                    }
                }
                None if !remaining.is_empty() => {
                    let kind = self.non_exhaustive(&matched, &remaining);
                    self.error(node_id, span, kind);
                    return None;
                }
                None => {}
            }
            let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &None)?;
            return Some(MatchShape {
                arms: checked_arms,
                else_branch: None,
                remainder: CheckedMatchRemainder::IllegalEnumTag,
                result_type,
            });
        }

        let else_branch = match &m.else_branch {
            Some(block) => Some(self.analyze_block(block, expected)?),
            None if !remaining.is_empty() => {
                let kind = self.non_exhaustive(&matched, &remaining);
                self.error(node_id, span, kind);
                return None;
            }
            None => None,
        };
        let result_type = self.unify_match_arm_types(node_id, span, &checked_arms, &else_branch)?;
        Some(MatchShape {
            arms: checked_arms,
            else_branch,
            remainder: if tests_tag {
                CheckedMatchRemainder::IllegalEnumTag
            } else {
                CheckedMatchRemainder::Covered
            },
            result_type,
        })
    }

    /// Rules 1, 2 and 5 between every pair of arms. Arms that nest in
    /// opposite directions on different components carry the same weight and
    /// may overlap; the earlier one wins there.
    fn check_arm_order(&mut self, node_id: HirId, arms: &[ResolvedArm<'_>]) -> bool {
        let mut ordered = true;
        for (index, later) in arms.iter().enumerate() {
            // A redundant `..` is reported by the coverage walk instead.
            if later.catch_all {
                continue;
            }
            for earlier in &arms[..index] {
                let previous = earlier.arm.pattern.span();
                let kind = match space::relate_arms(&earlier.space, &later.space) {
                    Relation::Partial(path) => AnalysisErrorKind::OverlappingMatchArm {
                        previous,
                        field: (!path.is_empty()).then(|| path.join(".")),
                    },
                    Relation::Equal => AnalysisErrorKind::DuplicateMatchArm { previous },
                    Relation::Super => AnalysisErrorKind::MoreSpecificArmAfterGeneral {
                        general: previous,
                        general_is_catch_all: earlier.catch_all,
                    },
                    Relation::Disjoint | Relation::Sub | Relation::SameWeight => continue,
                };
                self.error(node_id, later.arm.pattern.span(), kind);
                ordered = false;
                break;
            }
        }
        ordered
    }

    /// The scrutinee variants or members that still have uncovered values.
    fn root_alternatives(&self, remaining: &[Pat], matched: &ResolvedType) -> Vec<usize> {
        let count = match matched {
            ResolvedType::Enum { nominal, .. } => nominal.variant_names.len(),
            ResolvedType::AnonymousEnum { shape, .. } => shape.members().len(),
            _ => return Vec::new(),
        };
        let mut alternatives: Vec<usize> = remaining
            .iter()
            .flat_map(|cell| match cell {
                Pat::Variant { index, .. } | Pat::Member { index, .. } => vec![*index],
                _ => (0..count).collect(),
            })
            .collect();
        alternatives.sort_unstable();
        alternatives.dedup();
        alternatives
    }

    fn root_tag_groups(
        &mut self,
        node_id: HirId,
        root: &Position,
        alternatives: &[usize],
        span: Span,
    ) -> Option<Vec<Vec<CheckedExprNode>>> {
        let mut groups = Vec::with_capacity(alternatives.len());
        for &index in alternatives {
            let condition = match &root.r#type {
                ResolvedType::Enum { nominal, .. } => {
                    let nominal = nominal.clone();
                    let mut projections = root.place.projections.clone();
                    let tag_type = self.resolve_field_projection(
                        node_id,
                        span,
                        &mut projections,
                        &root.r#type,
                        &Ident("tag".to_string()),
                        Origin::default(),
                        &mut false,
                    )?;
                    let tag_place = CheckedPlace {
                        root: root.place.root.clone(),
                        projections,
                        r#type: tag_type.clone(),
                    };
                    let tag = self.type_facts(node_id, span, |types| {
                        Ok(types.enum_facts(&nominal)?.variants[index].tag)
                    })?;
                    self.tag_variant_condition(&tag_place, &tag_type, tag, span)
                }
                _ => {
                    let tag_type = crate::layout::ANONYMOUS_ENUM_TAG_TYPE;
                    let mut tag_place = root.place.clone();
                    tag_place.projections.push(CheckedProjection::EnumTag {
                        r#type: tag_type.clone(),
                    });
                    tag_place.r#type = tag_type.clone();
                    self.member_tag_condition(&tag_place, &tag_type, index, span)
                }
            };
            groups.push(vec![condition]);
        }
        Some(groups)
    }

    /// A flat remainder keeps the per-kind diagnostic it always had; only a
    /// remainder with nested structure is rendered as patterns.
    fn non_exhaustive(&self, matched: &ResolvedType, remaining: &[Pat]) -> AnalysisErrorKind {
        let flat = remaining.iter().all(|cell| match cell {
            Pat::Any | Pat::Range { .. } => true,
            Pat::Variant { fields, .. } => fields.components().is_empty(),
            Pat::Member { inner, .. } => matches!(**inner, Pat::Any),
            Pat::Struct { .. } => false,
        });
        match matched {
            ResolvedType::Enum { nominal, .. } if flat => {
                AnalysisErrorKind::NonExhaustiveMatchEnum {
                    r#enum: nominal.name.clone(),
                    missing: self
                        .root_alternatives(remaining, matched)
                        .into_iter()
                        .map(|index| nominal.variant_names[index].clone())
                        .collect(),
                }
            }
            ResolvedType::AnonymousEnum { shape, .. } if flat => {
                AnalysisErrorKind::NonExhaustiveMatchAnonymousEnum {
                    r#enum: matched.clone(),
                    missing: self
                        .root_alternatives(remaining, matched)
                        .into_iter()
                        .map(|index| shape.members()[index].clone())
                        .collect(),
                }
            }
            ResolvedType::Enum { .. }
            | ResolvedType::AnonymousEnum { .. }
            | ResolvedType::Struct(_) => AnalysisErrorKind::NonExhaustiveMatchPatterns {
                r#type: matched.clone(),
                missing: remaining.iter().map(space::render).collect(),
            },
            _ => {
                let (lo, hi) = matched
                    .integer_domain(self.target.pointer_bits())
                    .expect("an integral scrutinee has an integer domain");
                let kind = match matched {
                    ResolvedType::Bool => space::LeafKind::Bool,
                    ResolvedType::Char => space::LeafKind::Char,
                    _ => space::LeafKind::Integer,
                };
                AnalysisErrorKind::NonExhaustiveMatchValue {
                    r#type: matched.clone(),
                    gaps: remaining
                        .iter()
                        .map(|cell| match cell {
                            Pat::Any => space::render_interval(kind, lo, hi),
                            cell => space::render(cell),
                        })
                        .collect(),
                }
            }
        }
    }

    /// Refines the scrutinee binding for one arm's body: the arm's own proof
    /// joins whatever an enclosing arm already proved about the same binding.
    fn narrow_scrutinee_binding(
        &mut self,
        binding: &(Ident, Origin, HirId, Storage, bool),
        scrutinee_type: &ResolvedType,
        through_pointer: Option<bool>,
        proved: &[RefinedPath],
        span: Span,
    ) {
        let (ident, origin, decl_id, storage, mutable) = binding;
        let mut refinements: Vec<RefinedPath> = self
            .context
            .refinements(*decl_id)
            .iter()
            .filter(|outer| !proved.iter().any(|refined| refined.path == outer.path))
            .cloned()
            .collect();
        let mut root = None;
        for refined in proved {
            if refined.path.is_empty() {
                root = Some(refined.r#type.clone());
                if refined.member.is_none() {
                    continue;
                }
            }
            refinements.push(refined.clone());
        }
        let narrowed = match root {
            Some(refined) => match through_pointer {
                Some(pointer_mutable) => ResolvedType::Pointer {
                    pointee: Box::new(refined),
                    mutable: pointer_mutable,
                },
                None => refined,
            },
            None if refinements.is_empty() => return,
            None => scrutinee_type.clone(),
        };
        self.declare_narrowed_binding(
            *decl_id,
            span,
            ident,
            *origin,
            narrowed,
            *storage,
            *mutable,
            refinements,
        );
    }
    /// A `match` `else` on an enum scrutinee is an arm over the variants the
    /// other arms left, never an unconditional fallthrough: a tag outside the
    /// declared domain is a violated invariant, not one more alternative the
    /// user agreed to handle.
    ///
    /// With nothing left for it to mean, the block is dead. It is still
    /// checked for ordinary errors -- dead code is source the programmer
    /// wrote -- but its result joins nothing and its body is not emitted.
    fn analyze_else_arm(
        &mut self,
        node_id: HirId,
        block: &HirBlock,
        expected: Expected<'_>,
        reachable: bool,
    ) -> Option<CheckedBlock> {
        let checked = self.analyze_block(block, if reachable { expected } else { Expected::None });
        if !reachable {
            self.warn(node_id, block.span, AnalysisWarningKind::UnreachableCode);
            return None;
        }
        checked
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
