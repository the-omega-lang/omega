use super::*;

struct MemberOwner {
    visibility: Visibility,
    module_path: Vec<Ident>,
    id: HirId,
}

struct EnumMember {
    r#type: ResolvedType,
    projection: CheckedProjection,
    owner: Option<MemberOwner>,
}

impl<'r> Analyzer<'r> {
    pub(super) fn resolve_field_projection(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        current_type: &ResolvedType,
        field: &Ident,
        mutable: &mut bool,
    ) -> Option<ResolvedType> {
        let base = match current_type {
            ResolvedType::Pointer {
                pointee,
                mutable: pointer_mutable,
            } => {
                *mutable = *pointer_mutable;
                projections.push(CheckedProjection::Deref {
                    r#type: (**pointee).clone(),
                });
                (**pointee).clone()
            }
            other => other.clone(),
        };

        match &base {
            ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
                self.project_slice_field(node_id, span, projections, &base, field)
            }
            ResolvedType::SpecObject { mutable, .. } => {
                self.project_spec_object_field(node_id, span, projections, &base, field, *mutable)
            }
            ResolvedType::Enum { cell, variant } => {
                self.project_enum_field(node_id, span, projections, cell, *variant, &base, field)
            }
            ResolvedType::Union(cell) => {
                let cell = cell.clone();
                self.project_union_field(node_id, span, projections, &cell, &base, field)
            }
            ResolvedType::Struct(cell) => {
                let cell = cell.clone();
                self.project_struct_field(node_id, span, projections, &cell, &base, field)
            }
            _ => {
                self.error(node_id, span, AnalysisErrorKind::NotAStruct { found: base });
                None
            }
        }
    }

    fn project_slice_field(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        base: &ResolvedType,
        field: &Ident,
    ) -> Option<ResolvedType> {
        let expected_name = match base {
            ResolvedType::Str { .. } => "size",
            ResolvedType::Slice { .. } => "length",
            _ => unreachable!("resolve_field_projection only routes Slice/Str here"),
        };
        if field.as_ref() != expected_name {
            self.no_such_field(node_id, span, field, base);
            return None;
        }
        projections.push(CheckedProjection::SliceLength);
        Some(ResolvedType::I32)
    }

    fn project_spec_object_field(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        base: &ResolvedType,
        field: &Ident,
        mutable: bool,
    ) -> Option<ResolvedType> {
        match field.as_ref() {
            "ptr" => {
                projections.push(CheckedProjection::SpecObjectPtr { mutable });
                Some(ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::U8),
                    mutable,
                })
            }
            "vtable" => {
                projections.push(CheckedProjection::SpecObjectVtable);
                Some(ResolvedType::Pointer {
                    pointee: Box::new(ResolvedType::U8),
                    mutable: false,
                })
            }
            _ => {
                self.no_such_field(node_id, span, field, base);
                None
            }
        }
    }

    fn project_enum_field(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        variant: Option<usize>,
        base: &ResolvedType,
        field: &Ident,
    ) -> Option<ResolvedType> {
        // Borrow released before the `&mut self` call below.
        let member = Self::find_enum_member(&cell.borrow(), variant, field);
        let Some(member) = member else {
            let kind = Self::no_such_enum_field(&cell.borrow(), variant, field);
            self.error(node_id, span, kind);
            return None;
        };
        if let Some(owner) = member.owner {
            self.require_visible_member(node_id, span, field, base, owner)?;
        }
        projections.push(member.projection);
        Some(member.r#type)
    }

    fn find_enum_member(
        e: &ResolvedEnumType,
        variant: Option<usize>,
        field: &Ident,
    ) -> Option<EnumMember> {
        let owner = |visibility| MemberOwner {
            visibility,
            module_path: e.module_path.clone(),
            id: e.id,
        };

        if field.as_ref() == "tag" {
            let r#type = e.tag_type.clone();
            let projection = CheckedProjection::EnumTag {
                r#type: r#type.clone(),
            };
            return Some(EnumMember {
                r#type,
                projection,
                owner: None,
            });
        }
        if let Some((index, r#type, visibility)) = Self::find_field(&e.header, field) {
            let projection = CheckedProjection::EnumHeader {
                field: field.clone(),
                index,
                r#type: r#type.clone(),
            };
            return Some(EnumMember {
                r#type,
                projection,
                owner: Some(owner(visibility)),
            });
        }
        if let Some((index, r#type, visibility)) = Self::find_field(&e.dynamic_fields, field) {
            let projection = CheckedProjection::EnumDynamicField {
                field: field.clone(),
                index,
                r#type: r#type.clone(),
            };
            return Some(EnumMember {
                r#type,
                projection,
                owner: Some(owner(visibility)),
            });
        }
        let current = variant?;
        let (field_index, r#type, visibility) =
            Self::find_field(&e.variants[current].fields, field)?;
        let projection = CheckedProjection::EnumBody {
            variant_index: current,
            field_index,
            r#type: r#type.clone(),
        };
        Some(EnumMember {
            r#type,
            projection,
            owner: Some(owner(visibility)),
        })
    }

    fn no_such_enum_field(
        e: &ResolvedEnumType,
        variant: Option<usize>,
        field: &Ident,
    ) -> AnalysisErrorKind {
        let declaring = e
            .variants
            .iter()
            .find(|v| v.fields.iter().any(|(name, _, _)| name == field));
        match (declaring, variant) {
            (Some(declaring), Some(current)) => AnalysisErrorKind::EnumFieldWrongVariant {
                field: field.clone(),
                owner: declaring.name.clone(),
                actual: e.variants[current].name.clone(),
            },
            (Some(declaring), None) => AnalysisErrorKind::EnumFieldVariantUnknown {
                field: field.clone(),
                r#enum: e.name.clone(),
                owner: declaring.name.clone(),
            },
            (None, _) => {
                let tag = Ident("tag".into());
                let candidates = std::iter::once(&tag)
                    .chain(e.header.iter().map(|(name, _, _)| name))
                    .chain(e.dynamic_fields.iter().map(|(name, _, _)| name))
                    .chain(
                        variant
                            .iter()
                            .flat_map(|&i| e.variants[i].fields.iter().map(|(name, _, _)| name)),
                    );
                AnalysisErrorKind::NoSuchEnumField {
                    field: field.clone(),
                    r#enum: e.name.clone(),
                    similar: best_match(field, candidates),
                }
            }
        }
    }

    fn project_union_field(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        cell: &Rc<RefCell<ResolvedUnionType>>,
        base: &ResolvedType,
        field: &Ident,
    ) -> Option<ResolvedType> {
        let (found, owner_module, owner_id) = {
            let u = cell.borrow();
            (
                Self::find_field(&u.fields, field),
                u.module_path.clone(),
                u.id,
            )
        };
        let Some((index, r#type, visibility)) = found else {
            self.no_such_field(node_id, span, field, base);
            return None;
        };
        let owner = MemberOwner {
            visibility,
            module_path: owner_module,
            id: owner_id,
        };
        self.require_visible_member(node_id, span, field, base, owner)?;
        projections.push(CheckedProjection::UnionField {
            field: field.clone(),
            index,
            r#type: r#type.clone(),
        });
        Some(r#type)
    }

    fn project_struct_field(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        cell: &Rc<RefCell<ResolvedStructType>>,
        base: &ResolvedType,
        field: &Ident,
    ) -> Option<ResolvedType> {
        let (found, owner_module, owner_id) = {
            let s = cell.borrow();
            (
                Self::find_field(&s.fields, field),
                s.module_path.clone(),
                s.id,
            )
        };
        let Some((index, r#type, visibility)) = found else {
            self.no_such_field(node_id, span, field, base);
            return None;
        };
        let owner = MemberOwner {
            visibility,
            module_path: owner_module,
            id: owner_id,
        };
        self.require_visible_member(node_id, span, field, base, owner)?;
        projections.push(CheckedProjection::FieldAccess {
            field: field.clone(),
            index,
            r#type: r#type.clone(),
        });
        Some(r#type)
    }

    fn find_field(
        fields: &[(Ident, ResolvedType, Visibility)],
        name: &Ident,
    ) -> Option<(usize, ResolvedType, Visibility)> {
        fields
            .iter()
            .enumerate()
            .find(|(_, (field, _, _))| field == name)
            .map(|(index, (_, r#type, visibility))| (index, r#type.clone(), *visibility))
    }

    fn require_visible_member(
        &mut self,
        node_id: HirId,
        span: Span,
        field: &Ident,
        base: &ResolvedType,
        owner: MemberOwner,
    ) -> Option<()> {
        if self.check_member_visibility(owner.visibility, &owner.module_path, owner.id) {
            return Some(());
        }
        self.error(
            node_id,
            span,
            AnalysisErrorKind::FieldNotVisible {
                field: field.clone(),
                base: base.clone(),
            },
        );
        None
    }

    fn no_such_field(&mut self, node_id: HirId, span: Span, field: &Ident, base: &ResolvedType) {
        self.error(
            node_id,
            span,
            AnalysisErrorKind::NoSuchField {
                field: field.clone(),
                base: base.clone(),
            },
        );
    }

    pub(super) fn find_methods(
        &mut self,
        id: HirId,
        span: Span,
        current_type: &ResolvedType,
        field: &Ident,
    ) -> Vec<ResolvedMethod> {
        let current_type = current_type.autoderef();
        let mut methods = match current_type {
            ResolvedType::Struct(struct_type) => {
                let struct_type = struct_type.borrow();
                if struct_type.fields.iter().any(|(name, _, _)| name == field) {
                    return Vec::new();
                }
                struct_type
                    .functions
                    .iter()
                    .filter(|(name, _)| name == field)
                    .map(|(_, method)| method.clone())
                    .collect()
            }
            ResolvedType::Enum { cell, variant } => {
                let e = cell.borrow();
                let shadowed = field.as_ref() == "tag"
                    || e.header.iter().any(|(name, _, _)| name == field)
                    || variant.is_some_and(|i| {
                        e.variants[i]
                            .fields
                            .iter()
                            .any(|(name, _, _)| name == field)
                    });
                if shadowed {
                    return Vec::new();
                }
                e.functions
                    .iter()
                    .filter(|(name, _)| name == field)
                    .map(|(_, method)| method.clone())
                    .collect()
            }
            ResolvedType::Union(union_type) => {
                let union_type = union_type.borrow();
                if union_type.fields.iter().any(|(name, _, _)| name == field) {
                    return Vec::new();
                }
                union_type
                    .functions
                    .iter()
                    .filter(|(name, _)| name == field)
                    .map(|(_, method)| method.clone())
                    .collect()
            }
            other => match self.resolver.primitive_methods(other) {
                Ok(methods) => methods
                    .into_iter()
                    .filter(|(name, _)| name == field)
                    .map(|(_, m)| m)
                    .collect(),
                Err(err) => {
                    self.error(id, span, AnalysisErrorKind::ModuleResolution(err));
                    Vec::new()
                }
            },
        };

        for (target, spec, spec_args) in self.bounds.clone() {
            if target != *current_type {
                continue;
            }
            match self.resolver.conformance_for(current_type, &spec, &spec_args) {
                Ok(Some(conform)) => {
                    for method in conform
                        .methods
                        .into_iter()
                        .filter(|(name, _)| name == field)
                        .map(|(_, method)| method)
                    {
                        if !methods.iter().any(|existing| {
                            existing.decl_id == method.decl_id && existing.fn_type == method.fn_type
                        }) {
                            methods.push(method);
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => self.error(id, span, AnalysisErrorKind::ModuleResolution(err)),
            }
        }
        methods
    }

    pub(super) fn immutable_enum_member(target: &CheckedPlace) -> Option<Ident> {
        match target.projections.last()? {
            CheckedProjection::EnumTag { .. } => Some(Ident("tag".into())),
            CheckedProjection::EnumHeader { field, .. } => Some(field.clone()),
            _ => None,
        }
    }

    pub(super) fn places_provably_equal(a: &CheckedPlace, b: &CheckedPlace) -> bool {
        let roots_equal = match (&a.root, &b.root) {
            (
                CheckedPlaceRoot::Variable { decl_id: a, .. },
                CheckedPlaceRoot::Variable { decl_id: b, .. },
            ) => a == b,
            _ => false,
        };
        if !roots_equal || a.projections.len() != b.projections.len() {
            return false;
        }

        a.projections
            .iter()
            .zip(b.projections.iter())
            .all(|(a, b)| match (a, b) {
                (
                    CheckedProjection::FieldAccess { index: a, .. },
                    CheckedProjection::FieldAccess { index: b, .. },
                ) => a == b,
                (CheckedProjection::SliceLength, CheckedProjection::SliceLength) => true,
                (
                    CheckedProjection::SpecObjectPtr { .. },
                    CheckedProjection::SpecObjectPtr { .. },
                ) => true,
                (CheckedProjection::SpecObjectVtable, CheckedProjection::SpecObjectVtable) => true,
                (CheckedProjection::EnumTag { .. }, CheckedProjection::EnumTag { .. }) => true,
                (
                    CheckedProjection::EnumHeader { index: a, .. },
                    CheckedProjection::EnumHeader { index: b, .. },
                ) => a == b,
                (
                    CheckedProjection::EnumDynamicField { index: a, .. },
                    CheckedProjection::EnumDynamicField { index: b, .. },
                ) => a == b,
                (
                    CheckedProjection::EnumBody {
                        variant_index: av,
                        field_index: af,
                        ..
                    },
                    CheckedProjection::EnumBody {
                        variant_index: bv,
                        field_index: bf,
                        ..
                    },
                ) => av == bv && af == bf,
                (
                    CheckedProjection::UnionField { index: a, .. },
                    CheckedProjection::UnionField { index: b, .. },
                ) => a == b,
                _ => false,
            })
    }

    pub(super) fn require_mutable_place(
        &mut self,
        node_id: HirId,
        span: Span,
        hir_root: &HirPlaceRoot,
        checked_place: &CheckedPlace,
        mutable: bool,
    ) -> Option<()> {
        // Checked before the binding's own mutability, and here rather than
        // at any one write syntax: an enum's tag and header fields are
        // per-variant compile-time constants, so *no* write reaches them --
        // not `=`, not `+=`, not `&mut`, not a `mut self` method call. A
        // check at one of those sites only would leave the others able to
        // desynchronize a live value's tag from its actual variant.
        if let Some(field) = Self::immutable_enum_member(checked_place) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::EnumFieldImmutable { field },
            );
            return None;
        }
        if mutable {
            if let CheckedPlaceRoot::Variable { decl_id, .. } = checked_place.root {
                self.context.mark_written(decl_id);
            }
            return Some(());
        }
        let through_pointer = checked_place
            .projections
            .iter()
            .any(|p| matches!(p, CheckedProjection::Deref { .. }));
        let kind = if through_pointer {
            AnalysisErrorKind::NotMutablePointer
        } else if matches!(checked_place.root, CheckedPlaceRoot::Expr(_)) {
            AnalysisErrorKind::MutateTemporary
        } else {
            match hir_root {
                HirPlaceRoot::Path(p) if p.path.is_unqualified() => {
                    AnalysisErrorKind::NotMutableBinding {
                        ident: p.path.head.clone(),
                    }
                }
                _ => AnalysisErrorKind::NotMutablePointer,
            }
        };
        self.error(node_id, span, kind);
        None
    }

    pub(super) fn analyze_slice(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirPlace,
        range: &HirRange,
        requested_mutable: bool,
    ) -> Option<CheckedExprNode> {
        let (mut checked_base, base_type, place_mutable) =
            self.analyze_place(node_id, span, base, None)?;
        let base_type_snapshot = base_type.clone();
        let base_lacks_length = matches!(base_type_snapshot, ResolvedType::Array(_, _));

        // The slice's *source* mutability: for inline storage
        // (`SizedArray`), whether the storage being sliced is itself
        // writable (the place's own mutability); for re-slicing an
        // existing `Slice` value, that slice's own flag -- a property of
        // the value being sliced, independent of whether the *variable*
        // holding it happens to be `mut`. `requested_mutable` (from the
        // `&`/`&mut` the user actually wrote) may never exceed this: you
        // can't get a mutable slice out of an immutable array or an
        // already-immutable slice.
        // `is_str` tracks whether the *result* should be a `Str` instead of
        // a `Slice` -- re-slicing a `*str` produces another `*str`, not a
        // `*[u8]`, even though `item_type` (used for the byte-offset math
        // both share) is `U8` either way.
        let (item_type, source_mutable, from_fat_pointer, is_str) = match base_type {
            ResolvedType::SizedArray(item_type, _) => (*item_type, place_mutable, false, false),
            ResolvedType::Slice { item, mutable } => (*item, mutable, true, false),
            ResolvedType::Str { mutable } => (ResolvedType::U8, mutable, true, true),
            ResolvedType::Array(item, mutable) => (*item, mutable, true, false),
            found => {
                self.error(node_id, span, AnalysisErrorKind::NotSliceable { found });
                return None;
            }
        };
        if requested_mutable && !source_mutable {
            if from_fat_pointer {
                self.error(node_id, span, AnalysisErrorKind::ImmutableSliceSource);
                return None;
            }
            self.require_mutable_place(node_id, span, &base.root, &checked_base, source_mutable)?;
        }

        // A `comp`-bound `*[?]T` has no established promotion story: unlike
        // `SizedArray` (promote its address) or `Slice`/`Str` (already its
        // own two leaves), a `comp` array's own `ConstValue` shape isn't
        // one either of the two branches below already knows how to
        // handle, and speculatively guessing at one risks silently
        // building the wrong leaves rather than just rejecting a genuinely
        // narrow, likely-never-hit combination outright.
        if base_lacks_length
            && matches!(
                checked_base.root,
                CheckedPlaceRoot::Variable {
                    storage: Storage::Comp,
                    ..
                }
            )
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CompPointerSliceNotSupported,
            );
            return None;
        }

        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = checked_base.root
        {
            let value = self.resolve_comp_place(node_id, span, &checked_base)?;
            checked_base = if from_fat_pointer {
                let r#type = if is_str {
                    ResolvedType::Str { mutable: false }
                } else {
                    ResolvedType::Slice {
                        item: Box::new(item_type.clone()),
                        mutable: false,
                    }
                };
                CheckedPlace {
                    root: CheckedPlaceRoot::Expr(Box::new(CheckedExprNode {
                        id: node_id,
                        span,
                        r#type: r#type.clone(),
                        kind: CheckedExpr::Const(value),
                    })),
                    projections: vec![],
                    r#type,
                }
            } else {
                CheckedPlace {
                    root: CheckedPlaceRoot::Expr(Box::new(CheckedExprNode {
                        id: node_id,
                        span,
                        r#type: ResolvedType::Pointer {
                            pointee: Box::new(item_type.clone()),
                            mutable: false,
                        },
                        kind: CheckedExpr::Const(ConstValue::Ref(Box::new(value))),
                    })),
                    projections: vec![CheckedProjection::Deref {
                        r#type: base_type_snapshot.clone(),
                    }],
                    r#type: base_type_snapshot,
                }
            };
        }

        let analyze_bound = |this: &mut Self,
                             bound: Option<&HirExprNode>|
         -> Option<Option<Box<CheckedExprNode>>> {
            let Some(bound) = bound else {
                return Some(None);
            };
            let checked_bound = this.analyze_expr(bound, Some(&ResolvedType::I32))?;
            if checked_bound.r#type != ResolvedType::I32 {
                this.error(
                    bound.id,
                    bound.span,
                    AnalysisErrorKind::InvalidSliceBound {
                        r#type: checked_bound.r#type,
                    },
                );
                return None;
            }
            Some(Some(Box::new(checked_bound)))
        };

        let checked_start = analyze_bound(self, range.start.as_deref())?;
        let checked_end = analyze_bound(self, range.end.expr())?;
        // A missing `start` always defaults to `0`, fine for every base
        // kind -- but a missing `end` only has something to default to
        // when `base_lacks_length` is `false` (`SizedArray`'s compile-time
        // `N`, or `Slice`/`Str`'s own runtime length leaf). `*[?]T` (`Array`)
        // has no such fallback anywhere, so `&arr[a..]` must name its own
        // end explicitly; `&arr[a..<b]`/`&arr[..<b]` are unaffected.
        if checked_end.is_none() && base_lacks_length {
            self.error(node_id, span, AnalysisErrorKind::MissingSliceEnd);
            return None;
        }

        let result_type = if is_str {
            ResolvedType::Str {
                mutable: requested_mutable,
            }
        } else {
            ResolvedType::Slice {
                item: Box::new(item_type.clone()),
                mutable: requested_mutable,
            }
        };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: result_type,
            kind: CheckedExpr::Slice(CheckedSlice {
                base: checked_base,
                item_type,
                start: checked_start,
                end: checked_end,
                inclusive: range.inclusive(),
            }),
        })
    }

    pub(super) fn analyze_const_slice(
        &mut self,
        node_id: HirId,
        span: Span,
        elements: &[HirExprNode],
        mutable: bool,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        if mutable {
            self.error(node_id, span, AnalysisErrorKind::ConstSliceCannotBeMutable);
            return None;
        }
        if elements.is_empty() {
            self.error(node_id, span, AnalysisErrorKind::EmptyArrayLiteral);
            return None;
        }

        let item_type = match expected {
            Some(ResolvedType::Slice {
                item,
                mutable: false,
            }) => item.as_ref().clone(),
            _ => self.analyze_expr(&elements[0], None)?.r#type.widened(),
        };

        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.const_eval_slice(element, &item_type)?);
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Slice {
                item: Box::new(item_type),
                mutable: false,
            },
            kind: CheckedExpr::Const(ConstValue::Slice(values)),
        })
    }

    pub(super) fn analyze_place(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlace, ResolvedType, bool)> {
        let (root, mut current_type, mut mutable) =
            self.resolve_place_root(node_id, span, place, expected)?;

        // Any projection off a root *reads* that root, whether the place is
        // ultimately written or not: `*out = 5` must load `out` to know where
        // to store, and `s.v = 5` must load `s` to find the field. Only a
        // bare, projection-less `n = 5` is a pure write, and that case is
        // deliberately left alone so `UnusedVariable` still fires on a
        // genuinely write-only binding.
        //
        // Without this, every out-pointer parameter in the tree reported
        // `UnusedParameter` -- `List::pop(*mut self, out: *mut T)` writes
        // `*out` and still warned.
        if !place.projections.is_empty()
            && let CheckedPlaceRoot::Variable { decl_id, .. } = root
        {
            self.context.mark_used(decl_id);
        }

        let mut projections = Vec::with_capacity(place.projections.len());
        for projection in &place.projections {
            current_type = match projection {
                HirProjection::FieldAccess(field) => self.resolve_field_projection(
                    node_id,
                    span,
                    &mut projections,
                    &current_type,
                    field,
                    &mut mutable,
                )?,
                HirProjection::Index(index) => self.project_index(
                    node_id,
                    span,
                    &mut projections,
                    current_type,
                    index,
                    &mut mutable,
                )?,
                HirProjection::Deref => {
                    let ResolvedType::Pointer {
                        pointee,
                        mutable: pointer_mutable,
                    } = current_type
                    else {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::NotAPointer {
                                found: current_type,
                            },
                        );
                        return None;
                    };
                    mutable = pointer_mutable;
                    projections.push(CheckedProjection::Deref {
                        r#type: (*pointee).clone(),
                    });
                    *pointee
                }
            };
        }

        Some((
            CheckedPlace { root, projections, r#type: current_type.clone() },
            current_type,
            mutable,
        ))
    }

    fn resolve_place_root(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        match &place.root {
            HirPlaceRoot::Path(expr_path) if expr_path.plain().is_none() => {
                let (root, r#type) = self.resolve_generic_args_place(node_id, span, expr_path)?;
                Some((root, r#type, false))
            }
            HirPlaceRoot::Path(expr_path) if expr_path.path.is_unqualified() => {
                self.resolve_unqualified_root(
                    node_id,
                    span,
                    &expr_path.path.head,
                    expr_path.path.origin,
                    expected,
                )
            }
            HirPlaceRoot::Path(expr_path) => {
                let path = &expr_path.path;
                let alias = self.resolve_path_alias_or_error(node_id, span, path)?;
                let (root, r#type, mutable) = match alias {
                    Some(ImportTarget::Module(target)) => {
                        let absolute: Vec<Ident> = target
                            .into_iter()
                            .chain(path.tail.iter().cloned())
                            .collect();
                        self.resolve_qualified_value(
                            node_id,
                            span,
                            path,
                            &self.path_module(path),
                            absolute,
                            None,
                            expected,
                        )?
                    }
                    _ => {
                        let (root, r#type) =
                            self.resolve_type_qualified_value(node_id, span, path, expected)?;
                        (root, r#type, false)
                    }
                };
                Some((root, r#type, mutable))
            }
            HirPlaceRoot::Expr(expr) => {
                let checked = self.analyze_expr(expr, None)?;
                let r#type = checked.r#type.clone();
                Some((CheckedPlaceRoot::Expr(Box::new(checked)), r#type, false))
            }
        }
    }

    fn resolve_unqualified_root(
        &mut self,
        node_id: HirId,
        span: Span,
        ident: &Ident,
        origin: Origin,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        if let Some(binding) = self.context.find_variable(ident, origin) {
            let root = CheckedPlaceRoot::Variable {
                decl_id: binding.decl_id,
                storage: binding.storage,
                r#type: binding.r#type.clone(),
            };
            return Some((root, binding.r#type.clone(), binding.mutable));
        }

        if origin.0.is_none()
            && let Some((absolute, candidates)) = self.resolve_bare_overload_candidates(ident)
        {
            let (root, r#type) =
                self.resolve_bare_overload_root(node_id, span, &absolute, candidates, expected)?;
            return Some((root, r#type, false));
        }

        // An import alias, lazily resolved (see `resolve_alias`). A plain
        // item *value* alias resolves outright; a *generic* item alias takes
        // priority over the implicit own-module prefix, exactly like type
        // position does -- this is only ever reached for a *non-call*
        // reference to a generic function (a call goes through
        // `resolve_generic_call` first), which has no way to supply type
        // arguments, so `ensure_item` reports it uniformly as
        // `GenericArgCountMismatch` rather than silently matching an
        // unrelated same-named item in this module. A *type* alias or a bare
        // module alias referenced this way, like no alias at all, falls
        // through to the implicit own-module assumption --
        // `resolve_qualified_value` reports whichever precise error fits.
        let resolution_module = self
            .resolver
            .macro_origin_module(origin)
            .unwrap_or_else(|| self.module_path.clone());
        let alias = match self.resolver.resolve_import_alias(&resolution_module, ident) {
            Ok(alias) => alias,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return None;
            }
        };
        if let Some(ImportTarget::Item(
            _,
            ResolvedItem::Value {
                r#type,
                storage,
                decl_id,
                mutable,
            },
        )) = alias
        {
            let root = CheckedPlaceRoot::Variable {
                decl_id,
                storage,
                r#type: r#type.clone(),
            };
            return Some((root, r#type, mutable));
        }
        let (absolute, unqualified) = match alias {
            Some(ImportTarget::GenericItem(absolute)) => (absolute, None),
            _ => {
                let absolute = resolution_module
                    .iter()
                    .cloned()
                    .chain(std::iter::once(ident.clone()))
                    .collect();
                (absolute, Some(ident))
            }
        };
        self.resolve_qualified_value(
            node_id,
            span,
            &Path { head: ident.clone(), tail: vec![], origin },
            &resolution_module,
            absolute,
            unqualified,
            expected,
        )
    }

    fn resolve_bare_overload_root(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: &[Ident],
        candidates: OverloadCandidates,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let signatures: Vec<(HirId, ResolvedFunctionType)> = candidates
            .iter()
            .map(|(id, fn_type, _)| (*id, fn_type.clone()))
            .collect();
        let winner = match expected {
            Some(ResolvedType::Function(expected_fn)) => {
                Self::unique_overload_signature_match(expected_fn, &signatures)
            }
            _ => None,
        };
        let Some((decl_id, fn_type)) = winner else {
            let name = absolute
                .last()
                .expect("an absolute item path always ends in the item's own name");
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: candidates.into_iter().map(|(_, t, _)| t).collect(),
                },
            );
            return None;
        };
        let r#type = ResolvedType::Function(fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id,
            storage: Storage::Function,
            r#type: r#type.clone(),
        };
        Some((root, r#type))
    }

    fn project_index(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        current_type: ResolvedType,
        index: &HirExprNode,
        mutable: &mut bool,
    ) -> Option<ResolvedType> {
        let checked_index = self.analyze_expr(index, None)?;
        let item_type = match current_type {
            ResolvedType::SizedArray(item, _) => *item,
            ResolvedType::Array(item, array_mutable) => {
                *mutable = array_mutable;
                *item
            }
            ResolvedType::Slice {
                item,
                mutable: slice_mutable,
            } => {
                *mutable = slice_mutable;
                *item
            }
            ResolvedType::Str {
                mutable: str_mutable,
            } => {
                *mutable = str_mutable;
                ResolvedType::U8
            }
            found => {
                self.error(node_id, span, AnalysisErrorKind::NotAnArray { found });
                return None;
            }
        };
        projections.push(CheckedProjection::Index {
            index_expr: Box::new(checked_index),
            item_type: item_type.clone(),
        });
        Some(item_type)
    }
}
