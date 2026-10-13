use super::*;
use crate::analysis::literals::AggregateTarget;
use crate::exhaustiveness::{ComponentKey, Leaf, LeafKind, Pat, Product, Sum};
use omega_hir::HirFieldsPattern;

/// One matched position: the place that reads it, its type, and the
/// field-name path below the scrutinee that refinements are keyed by.
pub(super) struct Position {
    pub(super) place: CheckedPlace,
    pub(super) r#type: ResolvedType,
    pub(super) path: Vec<Ident>,
}

impl Position {
    fn projected(&self, projection: CheckedProjection, r#type: ResolvedType) -> CheckedPlace {
        let mut place = self.place.clone();
        place.projections.push(projection);
        place.r#type = r#type;
        place
    }
}

/// What one arm's pattern proves. `conditions` run outer to inner, so every
/// tag test precedes the projections into the variant it selects.
#[derive(Default)]
pub(super) struct Proof {
    pub(super) conditions: Vec<CheckedExprNode>,
    pub(super) refinements: Vec<RefinedPath>,
    pub(super) tests_tag: bool,
}

impl Proof {
    fn refine(&mut self, path: &[Ident], r#type: ResolvedType) {
        self.refinements.push(RefinedPath {
            path: path.to_vec(),
            r#type,
            member: None,
        });
    }
}

impl<'r> Analyzer<'r> {
    pub(super) fn is_patternable(&self, r#type: &ResolvedType) -> bool {
        match r#type {
            ResolvedType::Enum { .. }
            | ResolvedType::AnonymousEnum { .. }
            | ResolvedType::Struct(_) => true,
            other => other.integer_domain(self.target.pointer_bits()).is_some(),
        }
    }

    /// A nested bare `..` leaves its position unconstrained; only an arm's
    /// top-level `..` is the catch-all arm, and the caller handles that.
    pub(super) fn resolve_subpattern(
        &mut self,
        node_id: HirId,
        position: &Position,
        pattern: &HirPattern,
        proof: &mut Proof,
    ) -> Option<Pat> {
        if pattern.catch_all_range().is_some() {
            return Some(Pat::Any);
        }
        match &position.r#type {
            ResolvedType::Enum { nominal, .. } => {
                let nominal = nominal.clone();
                self.resolve_enum_subpattern(node_id, position, &nominal, pattern, proof)
            }
            ResolvedType::AnonymousEnum { shape, .. } => {
                let shape = shape.clone();
                self.resolve_anonymous_subpattern(node_id, position, &shape, pattern, proof)
            }
            ResolvedType::Struct(nominal) => {
                let nominal = nominal.clone();
                self.resolve_struct_subpattern(node_id, position, &nominal, pattern, proof)
            }
            _ => self.resolve_integral_subpattern(node_id, position, pattern, proof),
        }
    }

    fn resolve_integral_subpattern(
        &mut self,
        node_id: HirId,
        position: &Position,
        pattern: &HirPattern,
        proof: &mut Proof,
    ) -> Option<Pat> {
        let r#type = &position.r#type;
        let domain = r#type
            .integer_domain(self.target.pointer_bits())
            .expect("only patternable positions are resolved");
        let value = match &pattern.value {
            Some(HirPatternValue::Fields(fields)) => {
                self.error(
                    node_id,
                    fields.span,
                    AnalysisErrorKind::PatternPathMismatch {
                        expected: r#type.clone(),
                        found: Some(Self::path_text(&fields.path)),
                    },
                );
                return None;
            }
            Some(value) => value,
            None => {
                self.error(
                    node_id,
                    pattern.span(),
                    AnalysisErrorKind::PatternValueNotConstant,
                );
                return None;
            }
        };
        let (lo, hi, conditions) = self.analyze_value_pattern(value, r#type, &position.place)?;
        proof.conditions.extend(conditions);
        let kind = match r#type {
            ResolvedType::Bool => LeafKind::Bool,
            ResolvedType::Char => LeafKind::Char,
            _ => LeafKind::Integer,
        };
        Some(Pat::Range {
            lo,
            hi,
            leaf: Rc::new(Leaf {
                lo: domain.0,
                hi: domain.1,
                kind,
            }),
        })
    }

    fn resolve_enum_subpattern(
        &mut self,
        node_id: HirId,
        position: &Position,
        nominal: &NominalRef,
        pattern: &HirPattern,
        proof: &mut Proof,
    ) -> Option<Pat> {
        let (index, fields) = match &pattern.value {
            Some(HirPatternValue::Value(expr)) => {
                (self.resolve_variant_pattern(nominal, expr)?, None)
            }
            Some(HirPatternValue::Fields(fields)) => (
                self.resolve_variant_path(node_id, fields.span, nominal, &fields.path)?,
                Some(fields),
            ),
            _ => {
                self.error(
                    node_id,
                    pattern.span(),
                    AnalysisErrorKind::PatternNotEnumVariant {
                        r#enum: nominal.name.clone(),
                    },
                );
                return None;
            }
        };

        let mut tag_projections = position.place.projections.clone();
        let tag_type = self.resolve_field_projection(
            node_id,
            pattern.span(),
            &mut tag_projections,
            &position.r#type,
            &Ident("tag".to_string()),
            Origin::default(),
            &mut false,
        )?;
        let tag_place = CheckedPlace {
            root: position.place.root.clone(),
            projections: tag_projections,
            r#type: tag_type.clone(),
        };
        let e = self.type_facts(node_id, pattern.span(), |types| {
            types.enum_facts(nominal).cloned()
        })?;
        let tag_test = self.tag_variant_condition(
            &tag_place,
            &tag_type,
            e.variants[index].tag,
            pattern.span(),
        );
        proof.conditions.push(tag_test);
        proof.tests_tag = true;

        let refined = ResolvedType::Enum {
            nominal: nominal.clone(),
            variant: Some(index),
        };
        proof.refine(&position.path, refined.clone());

        let fields = match fields {
            Some(fields) => {
                let (declared, header, module_path) = {
                    let declared: Vec<ResolvedField> = e
                        .dynamic_fields
                        .iter()
                        .chain(&e.variants[index].fields)
                        .cloned()
                        .collect();
                    let header: Vec<Ident> = std::iter::once(Ident("tag".to_string()))
                        .chain(e.header.iter().map(|field| field.name.clone()))
                        .collect();
                    (declared, header, nominal.module_path.clone())
                };
                self.resolve_field_subpatterns(
                    node_id,
                    position,
                    &refined,
                    fields,
                    &declared,
                    &module_path,
                    &header,
                    proof,
                )?
            }
            None => Product::default(),
        };
        let sum = Sum {
            names: e
                .variants
                .iter()
                .map(|v| format!("{}::{}", nominal.name.as_ref(), v.name.as_ref()))
                .collect(),
        };
        Some(Pat::Variant {
            sum: Rc::new(sum),
            index,
            fields,
        })
    }

    fn resolve_anonymous_subpattern(
        &mut self,
        node_id: HirId,
        position: &Position,
        shape: &Rc<ResolvedAnonymousEnum>,
        pattern: &HirPattern,
        proof: &mut Proof,
    ) -> Option<Pat> {
        let (index, describes_member) =
            self.resolve_anonymous_member(node_id, shape, &position.r#type, pattern)?;
        let member = shape.members()[index].clone();

        let tag_type = crate::layout::ANONYMOUS_ENUM_TAG_TYPE;
        let tag_place = position.projected(
            CheckedProjection::EnumTag {
                r#type: tag_type.clone(),
            },
            tag_type.clone(),
        );
        let tag_test = self.member_tag_condition(&tag_place, &tag_type, index, pattern.span());
        proof.conditions.push(tag_test);
        proof.tests_tag = true;

        let mut member_refinement = None;
        let inner = if describes_member {
            let member_position = Position {
                place: position.projected(
                    CheckedProjection::EnumBody {
                        variant_index: index,
                        field_index: 0,
                        r#type: member.clone(),
                    },
                    member.clone(),
                ),
                r#type: member.clone(),
                path: position.path.clone(),
            };
            let before = proof.refinements.len();
            let inner = self.resolve_subpattern(node_id, &member_position, pattern, proof)?;
            // The member's own variant proof shares this position's path; it
            // rides along on the member refinement instead of shadowing it.
            if let Some(offset) = proof.refinements[before..]
                .iter()
                .position(|refined| refined.path == position.path)
            {
                member_refinement = Some(proof.refinements.remove(before + offset).r#type);
            }
            inner
        } else {
            Pat::Any
        };

        proof.refinements.push(RefinedPath {
            path: position.path.clone(),
            r#type: ResolvedType::AnonymousEnum {
                shape: shape.clone(),
                variant: Some(index),
            },
            member: member_refinement,
        });
        let sum = Sum {
            names: shape.members().iter().map(|m| m.to_string()).collect(),
        };
        Some(Pat::Member {
            sum: Rc::new(sum),
            index,
            inner: Box::new(inner),
        })
    }

    /// Which member an anonymous-enum subpattern selects, and whether the
    /// pattern also describes that member's own shape. A member type selects
    /// the member alone; a struct pattern or variant path selects the member
    /// it describes.
    fn resolve_anonymous_member(
        &mut self,
        node_id: HirId,
        shape: &ResolvedAnonymousEnum,
        parent: &ResolvedType,
        pattern: &HirPattern,
    ) -> Option<(usize, bool)> {
        if let Some(HirPatternValue::Value(expr)) = &pattern.value {
            return self.with_reveal_operand(expr, |this, _| {
                this.resolve_anonymous_member_header(node_id, shape, parent, pattern)
            });
        }
        self.resolve_anonymous_member_header(node_id, shape, parent, pattern)
    }

    fn resolve_anonymous_member_header(
        &mut self,
        node_id: HirId,
        shape: &ResolvedAnonymousEnum,
        parent: &ResolvedType,
        pattern: &HirPattern,
    ) -> Option<(usize, bool)> {
        let (path, span) = match &pattern.value {
            Some(HirPatternValue::Fields(fields)) => (&fields.path, fields.span),
            Some(HirPatternValue::Value(expr)) => {
                let Some(path) = Self::variant_shaped_path(expr) else {
                    return self
                        .resolve_anonymous_member_type(node_id, pattern, shape, parent)
                        .map(|index| (index, false));
                };
                if let Some(raw_type) = &pattern.r#type {
                    if let Some(member) = self.without_diagnostics(|analyzer| {
                        analyzer.resolve_type_or_error(node_id, pattern.span(), raw_type, false)
                    }) {
                        let describes_member = matches!(
                            member,
                            ResolvedType::Enum {
                                variant: Some(_),
                                ..
                            }
                        );
                        return self
                            .pattern_member_index(
                                node_id,
                                pattern.span(),
                                shape,
                                parent,
                                member.widened(),
                            )
                            .map(|index| (index, describes_member));
                    }
                }
                (path, expr.span)
            }
            _ => {
                return self
                    .resolve_anonymous_member_type(node_id, pattern, shape, parent)
                    .map(|index| (index, false));
            }
        };
        let target = self.resolve_aggregate_path(node_id, span, path, &[], Expected::None)?;
        self.pattern_member_index(node_id, span, shape, parent, target.r#type().widened())
            .map(|index| (index, true))
    }

    fn pattern_member_index(
        &mut self,
        node_id: HirId,
        span: Span,
        shape: &ResolvedAnonymousEnum,
        parent: &ResolvedType,
        member: ResolvedType,
    ) -> Option<usize> {
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

    fn variant_shaped_path(expr: &HirExprNode) -> Option<&ExprPath> {
        match &Self::strip_reveal(expr).1.expr {
            HirExpr::Place(HirPlace {
                root: HirPlaceRoot::Path(path),
                projections,
            }) if projections.is_empty() && !path.path.tail.is_empty() => Some(path),
            _ => None,
        }
    }

    pub(super) fn path_text(path: &ExprPath) -> String {
        path.path
            .segments()
            .iter()
            .map(|segment| segment.as_ref())
            .collect::<Vec<_>>()
            .join("::")
    }

    fn resolve_struct_subpattern(
        &mut self,
        node_id: HirId,
        position: &Position,
        nominal: &NominalRef,
        pattern: &HirPattern,
        proof: &mut Proof,
    ) -> Option<Pat> {
        let fields = match &pattern.value {
            Some(HirPatternValue::Fields(fields)) => fields,
            _ => {
                self.error(
                    node_id,
                    pattern.span(),
                    AnalysisErrorKind::PatternPathMismatch {
                        expected: position.r#type.clone(),
                        found: None,
                    },
                );
                return None;
            }
        };
        let target = self.resolve_aggregate_path(
            node_id,
            fields.span,
            &fields.path,
            &[],
            Expected::Exact(&position.r#type),
        )?;
        if target.r#type() != position.r#type.widened() {
            self.error(
                node_id,
                fields.span,
                AnalysisErrorKind::PatternTypeMismatch {
                    expected: position.r#type.clone(),
                    found: target.r#type(),
                },
            );
            return None;
        }
        let declared = self.type_facts(node_id, fields.span, |types| {
            types.fields(nominal).map(<[_]>::to_vec)
        })?;
        let module_path = nominal.module_path.clone();
        let fields = self.resolve_field_subpatterns(
            node_id,
            position,
            &position.r#type.clone(),
            fields,
            &declared,
            &module_path,
            &[],
            proof,
        )?;
        Some(Pat::Struct {
            name: Rc::from(nominal.name.as_ref()),
            fields,
        })
    }

    /// The fields a pattern may name mirror the ones construction supplies:
    /// runtime fields only, never the tag or a header field.
    fn resolve_field_subpatterns(
        &mut self,
        node_id: HirId,
        position: &Position,
        owner: &ResolvedType,
        pattern: &HirFieldsPattern,
        declared: &[ResolvedField],
        declaring_module: &ModulePath,
        header: &[Ident],
        proof: &mut Proof,
    ) -> Option<Product> {
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        let mut components = Vec::with_capacity(pattern.fields.len());
        for field in &pattern.fields {
            if let Some(previous) = seen.insert(field.name.clone(), field.name_span) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::PatternDuplicateField {
                        field: field.name.clone(),
                        previous,
                    },
                );
                return None;
            }
            let Some((index, declared_field)) = declared
                .iter()
                .enumerate()
                .find(|(_, declared)| declared.name == field.name)
            else {
                let kind = if header.contains(&field.name) {
                    AnalysisErrorKind::PatternHeaderField {
                        field: field.name.clone(),
                    }
                } else if let ResolvedType::Enum { nominal, .. } = owner {
                    AnalysisErrorKind::NoSuchEnumField {
                        field: field.name.clone(),
                        r#enum: nominal.name.clone(),
                        similar: best_match(&field.name, declared.iter().map(|d| &d.name)),
                    }
                } else {
                    AnalysisErrorKind::NoSuchField {
                        field: field.name.clone(),
                        base: owner.clone(),
                    }
                };
                self.error(node_id, field.name_span, kind);
                return None;
            };
            if !self.check_visibility(
                declared_field.visibility,
                declaring_module,
                field.name_origin,
            ) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::FieldNotVisible {
                        field: field.name.clone(),
                        base: owner.clone(),
                    },
                );
                return None;
            }
            if !self.is_patternable(&declared_field.r#type) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::PatternFieldNotPatternable {
                        field: field.name.clone(),
                        r#type: declared_field.r#type.clone(),
                    },
                );
                return None;
            }

            let mut projections = position.place.projections.clone();
            let field_type = self.resolve_field_projection(
                node_id,
                field.name_span,
                &mut projections,
                owner,
                &field.name,
                field.name_origin,
                &mut false,
            )?;
            let mut path = position.path.clone();
            path.push(field.name.clone());
            let child = Position {
                place: CheckedPlace {
                    root: position.place.root.clone(),
                    projections,
                    r#type: field_type.clone(),
                },
                r#type: field_type,
                path,
            };
            let pat = self.resolve_subpattern(node_id, &child, &field.pattern, proof)?;
            components.push((
                ComponentKey {
                    position: index,
                    name: field.name.as_ref().to_string(),
                },
                pat,
            ));
        }

        if pattern.rest.is_none() {
            let origin = pattern.path.path.origin;
            let mut missing = Vec::new();
            for declared in declared {
                if !seen.contains_key(&declared.name)
                    && self.is_patternable(&declared.r#type)
                    && self.check_visibility(declared.visibility, declaring_module, origin)
                {
                    missing.push(declared.name.clone());
                }
            }
            if !missing.is_empty() {
                self.error(
                    node_id,
                    pattern.span,
                    AnalysisErrorKind::PatternMissingFields { missing },
                );
                return None;
            }
        }
        Some(Product::new(components))
    }

    /// An anonymous-enum arm names a member type. The parser kept a type
    /// reading of the pattern only when the whole pattern parsed as one, so a
    /// missing candidate here means the arm was never spelled as a type.
    fn resolve_anonymous_member_type(
        &mut self,
        node_id: HirId,
        pattern: &HirPattern,
        shape: &ResolvedAnonymousEnum,
        parent: &ResolvedType,
    ) -> Option<usize> {
        let span = pattern.span();
        let Some(raw_type) = &pattern.r#type else {
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
        self.pattern_member_index(node_id, span, shape, parent, member)
    }

    pub(super) fn member_tag_condition(
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

    pub(super) fn tag_variant_condition(
        &mut self,
        tag_place: &CheckedPlace,
        tag_type: &ResolvedType,
        tag: NumberValue,
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

    fn resolve_variant_pattern(
        &mut self,
        nominal: &NominalRef,
        expr: &HirExprNode,
    ) -> Option<usize> {
        self.with_reveal_operand(expr, |this, expr| {
            let Some(path) = Self::variant_shaped_path(expr) else {
                this.error(
                    expr.id,
                    expr.span,
                    AnalysisErrorKind::PatternNotEnumVariant {
                        r#enum: nominal.name.clone(),
                    },
                );
                return None;
            };
            this.resolve_variant_path(expr.id, expr.span, nominal, path)
        })
    }

    fn resolve_variant_path(
        &mut self,
        node_id: HirId,
        span: Span,
        nominal: &NominalRef,
        expr_path: &ExprPath,
    ) -> Option<usize> {
        let expected = ResolvedType::Enum {
            nominal: nominal.clone(),
            variant: None,
        };
        let target =
            self.resolve_aggregate_path(node_id, span, expr_path, &[], Expected::Exact(&expected))?;
        match target {
            AggregateTarget::EnumVariant(found, index)
                if ResolvedType::Enum {
                    nominal: found.clone(),
                    variant: None,
                } == expected =>
            {
                Some(index)
            }
            AggregateTarget::EnumVariant(found, index) => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::PatternTypeMismatch {
                        expected,
                        found: ResolvedType::Enum {
                            nominal: found,
                            variant: Some(index),
                        },
                    },
                );
                None
            }
            _ => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::PatternNotEnumVariant {
                        r#enum: nominal.name.clone(),
                    },
                );
                None
            }
        }
    }

    fn analyze_value_pattern(
        &mut self,
        value: &HirPatternValue,
        scrutinee_type: &ResolvedType,
        scrutinee_place: &CheckedPlace,
    ) -> Option<(i128, i128, Vec<CheckedExprNode>)> {
        match value {
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
            HirPatternValue::Fields(_) => {
                unreachable!("the caller rejects a field pattern at an integral position")
            }
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
            | ConstValue::Ref(_)
            | ConstValue::Reflected(_) => {
                unreachable!("value patterns only ever sit at an integer/bool/char position")
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
            | ConstValue::Ref(_)
            | ConstValue::Reflected(_) => {
                unreachable!("value patterns only ever sit at an integer/bool/char position")
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
}
