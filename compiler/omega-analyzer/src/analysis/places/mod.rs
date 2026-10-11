use super::*;

mod fields;
mod roots;
mod slicing;

impl<'r> Analyzer<'r> {
    /// The functions `current_type` offers under `field` in one namespace.
    ///
    /// Instance syntax asks for [`FunctionNamespace::Member`] directly, so a
    /// receiverless declaration never enters an instance overload set; the
    /// static namespace is only probed to diagnose `value.static_name(...)`.
    /// A field of the same name shadows every function, in both namespaces.
    pub(super) fn find_functions(
        &mut self,
        id: HirId,
        span: Span,
        current_type: &ResolvedType,
        field: &Ident,
        namespace: FunctionNamespace,
    ) -> Vec<ResolvedMethod> {
        let current_type = current_type.autoderef();
        let mut methods = match current_type {
            ResolvedType::Struct(struct_type) => {
                let struct_type = struct_type.borrow();
                if struct_type
                    .fields
                    .iter()
                    .any(|candidate| &candidate.name == field)
                {
                    return Vec::new();
                }
                namespace.select(&struct_type.functions, field)
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
                namespace.select(&e.functions, field)
            }
            ResolvedType::Union(union_type) => {
                let union_type = union_type.borrow();
                if union_type
                    .fields
                    .iter()
                    .any(|candidate| &candidate.name == field)
                {
                    return Vec::new();
                }
                namespace.select(&union_type.functions, field)
            }
            other => match self.resolver.primitive_methods(other) {
                Ok(methods) => namespace.select(&methods, field),
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
                    for method in namespace.select(&conform.methods, field) {
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

    /// A field projected from a refined anonymous member must see that
    /// member's own proved variant, which only the refinement records.
    fn open_refined_member(
        refinements: &[RefinedPath],
        path: &[Ident],
        projections: &mut Vec<CheckedProjection>,
        current_type: ResolvedType,
        mutable: &mut bool,
    ) -> ResolvedType {
        let Some(member) = refinements
            .iter()
            .find(|r| r.path == path)
            .and_then(|r| r.member.clone())
        else {
            return current_type;
        };
        match Self::open_refined_anonymous(projections, &current_type, mutable) {
            Some(_) => member,
            None => current_type,
        }
    }

    /// The narrowable binding a place starts at and the field-name path it
    /// reaches, which is how refinements are keyed. An index ends the path:
    /// no element of an array is ever refined.
    pub(super) fn refinable_path(&self, place: &HirPlace) -> Option<(HirId, Vec<Ident>)> {
        let root = HirPlace {
            root: place.root.clone(),
            projections: Vec::new(),
        };
        let (_, _, decl_id, ..) = self.narrowable_place(&root)?;
        let path = place
            .projections
            .iter()
            .filter(|projection| !matches!(projection, HirProjection::Deref))
            .map_while(|projection| match projection {
                HirProjection::FieldAccess(field, ..) => Some(field.clone()),
                _ => None,
            })
            .collect();
        Some((decl_id, path))
    }

    /// `&mut place`, or anything else that hands out a mutable alias to it,
    /// gives up every proof at and below the place.
    pub(super) fn widen_place(&mut self, place: &HirPlace) {
        if let Some((ident, origin, ..)) = self.narrowable_place(place) {
            self.context.widen_variable(&ident, origin);
        } else if let Some((decl_id, path)) = self.refinable_path(place) {
            self.context.drop_refinements(decl_id, &path);
        }
    }

    /// Whether `place` is a refined field path itself, so a pointer to it
    /// must not carry the proof past the arm that made it.
    pub(super) fn is_refined_path(&self, place: &HirPlace) -> bool {
        self.refinable_path(place).is_some_and(|(decl_id, path)| {
            !path.is_empty()
                && self
                    .context
                    .refinements(decl_id)
                    .iter()
                    .any(|refined| refined.path == path)
        })
    }

    /// A proved path, or any prefix of it, is not an assignment target while
    /// the proof holds: the write would replace what the arm relies on.
    /// Writing *inside* a refined field leaves the proof intact.
    pub(super) fn require_unrefined_target(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
    ) -> Option<()> {
        let Some((decl_id, path)) = self.refinable_path(place) else {
            return Some(());
        };
        let Some(refined) = self
            .context
            .refinements(decl_id)
            .iter()
            .find(|refined| refined.path.starts_with(&path))
            .map(|refined| refined.path.clone())
        else {
            return Some(());
        };
        let HirPlaceRoot::Path(root) = &place.root else {
            unreachable!("a refinable place is rooted at a binding")
        };
        let render = |path: &[Ident]| {
            std::iter::once(root.path.head.as_ref())
                .chain(path.iter().map(|field| field.as_ref()))
                .collect::<Vec<_>>()
                .join(".")
        };
        self.error(
            node_id,
            span,
            AnalysisErrorKind::RefinedPathAssignment {
                target: render(&path),
                refined: render(&refined),
            },
        );
        None
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

        let refinements = match root {
            CheckedPlaceRoot::Variable { decl_id, .. } => {
                self.context.refinements(decl_id).to_vec()
            }
            _ => Vec::new(),
        };
        let mut refined_path = (!refinements.is_empty()).then(Vec::new);

        let mut projections = Vec::with_capacity(place.projections.len());
        for projection in &place.projections {
            if let (Some(path), HirProjection::FieldAccess(..)) = (&refined_path, projection) {
                current_type = Self::open_refined_member(
                    &refinements,
                    path,
                    &mut projections,
                    current_type,
                    &mut mutable,
                );
            }
            current_type = match projection {
                // Generic arguments are only ever written on a call's
                // member segment, which `resolve_callee` consumes before a
                // place is resolved from the projections before it.
                HirProjection::FieldAccess(field, origin, _) => self.resolve_field_projection(
                    node_id,
                    span,
                    &mut projections,
                    &current_type,
                    field,
                    *origin,
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
            match projection {
                HirProjection::FieldAccess(field, ..) => {
                    if let Some(path) = &mut refined_path {
                        path.push(field.clone());
                        if let Some(refined) = refinements.iter().find(|r| r.path == *path) {
                            current_type = refined.r#type.clone();
                        }
                    }
                }
                HirProjection::Index(_) => refined_path = None,
                HirProjection::Deref => {}
            }
        }

        Some((
            CheckedPlace {
                root,
                projections,
                r#type: current_type.clone(),
            },
            current_type,
            mutable,
        ))
    }
}
