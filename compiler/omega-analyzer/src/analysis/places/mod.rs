use super::*;

mod fields;
mod roots;
mod slicing;

impl<'r> Analyzer<'r> {
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
                if struct_type.fields.iter().any(|candidate| &candidate.name == field) {
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
                    || e.header.iter().any(|candidate| &candidate.name == field)
                    || variant.is_some_and(|i| {
                        e.variants[i]
                            .fields
                            .iter()
                            .any(|candidate| &candidate.name == field)
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
                if union_type.fields.iter().any(|candidate| &candidate.name == field) {
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

        for bound in self.bounds.clone() {
            if bound.target != *current_type {
                continue;
            }
            match self
                .resolver
                .conformance_for(current_type, &bound.spec, &bound.spec_args)
            {
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

}
