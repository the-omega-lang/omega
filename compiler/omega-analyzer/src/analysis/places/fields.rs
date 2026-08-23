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
    /// Opens a refined anonymous binding onto the member it proves, so that
    /// naming something *on* the member -- a field, an element, a method --
    /// reaches the payload. Whole-value operations (assignment, `&mut`,
    /// widening) deliberately never come through here and keep the anonymous
    /// root, which is what makes refinement free of representation effects.
    pub(crate) fn open_refined_anonymous(
        projections: &mut Vec<CheckedProjection>,
        current_type: &ResolvedType,
        mutable: &mut bool,
    ) -> Option<ResolvedType> {
        let (base, pointer_mutable) = match current_type {
            ResolvedType::Pointer {
                pointee,
                mutable: pointer_mutable,
            } => (&**pointee, Some(*pointer_mutable)),
            other => (other, None),
        };
        let (index, member) = base.refined_anonymous_member()?;
        let member = member.clone();
        if let Some(pointer_mutable) = pointer_mutable {
            *mutable = pointer_mutable;
            projections.push(CheckedProjection::Deref {
                r#type: base.clone(),
            });
        }
        projections.push(CheckedProjection::EnumBody {
            variant_index: index,
            field_index: 0,
            r#type: member.clone(),
        });
        Some(member)
    }

    pub(crate) fn resolve_field_projection(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        current_type: &ResolvedType,
        field: &Ident,
        mutable: &mut bool,
    ) -> Option<ResolvedType> {
        if let Some(member) = Self::open_refined_anonymous(projections, current_type, mutable) {
            return self.resolve_field_projection(
                node_id,
                span,
                projections,
                &member,
                field,
                mutable,
            );
        }
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
            ResolvedType::AnonymousEnum { .. } => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::AnonymousEnumNotRefined { r#enum: base },
                );
                None
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
            .find(|v| v.fields.iter().any(|candidate| &candidate.name == field));
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
                    .chain(e.header.iter().map(|field| &field.name))
                    .chain(e.dynamic_fields.iter().map(|field| &field.name))
                    .chain(
                        variant
                            .iter()
                            .flat_map(|&i| e.variants[i].fields.iter().map(|field| &field.name)),
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
        fields: &[ResolvedField],
        name: &Ident,
    ) -> Option<(usize, ResolvedType, Visibility)> {
        fields
            .iter()
            .enumerate()
            .find(|(_, field)| &field.name == name)
            .map(|(index, field)| (index, field.r#type.clone(), field.visibility))
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

    pub(super) fn project_index(
        &mut self,
        node_id: HirId,
        span: Span,
        projections: &mut Vec<CheckedProjection>,
        current_type: ResolvedType,
        index: &HirExprNode,
        mutable: &mut bool,
    ) -> Option<ResolvedType> {
        let checked_index = self.analyze_expr(index, None)?;
        let current_type = Self::open_refined_anonymous(projections, &current_type, mutable)
            .unwrap_or(current_type);
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
