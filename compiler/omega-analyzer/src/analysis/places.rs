use super::*;

/// Where a resolved member was declared, kept alongside its own visibility
/// so the access check can run after the declaring cell's borrow is
/// released.
struct MemberOwner {
    visibility: Visibility,
    module_path: Vec<Ident>,
    id: HirId,
}

/// One member reachable as `value.name` on an enum value: what it resolves
/// to, how to project it, and who declared it (`None` for the tag, which
/// has no declared visibility of its own).
struct EnumMember {
    r#type: ResolvedType,
    projection: CheckedProjection,
    owner: Option<MemberOwner>,
}

impl<'r> Analyzer<'r> {
    /// Resolves a single `.field` step against `current_type`, inserting a
    /// seamless one-level pointer deref first if needed (`ptr.field` is
    /// sugar for `(*ptr).field` when `ptr` points at an aggregate, matching
    /// Rust's autoderef -- exactly one level: `ptr.field` where `ptr` is
    /// `**Struct` still needs an explicit `(*ptr).field`).
    ///
    /// Shared by `analyze_place`'s projection loop and by member-call
    /// resolution, so plain field access and method access both get this
    /// from one implementation. `mutable`, like `analyze_place`'s own
    /// running mutability, is overwritten with the pointer's own flag when a
    /// seamless deref is inserted -- callers that don't care (a read, or a
    /// callable-field lookup) pass a throwaway `&mut bool`.
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
            // `slice.length` / `str.size` -- not a real field (neither is a
            // struct), so this is answered before the aggregate paths below
            // reject it.
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

    /// `.length` on a `*[?]T`, or `.size` on a `*str` -- both project to the
    /// exact same `CheckedProjection::SliceLength` marker (`Str` shares
    /// `Slice`'s exact fat-pointer leaf layout, `[ptr, len]`, so it's the
    /// same byte-count read either way), but the *name* a user spells it
    /// with deliberately differs: a slice's second leaf really is an
    /// element count, so "length" fits, but `*str`'s is a UTF-8 *byte*
    /// count -- "length" there would nudge a reader toward "character
    /// count", which it isn't. Using the *other* type's name (`.size` on a
    /// slice, `.length` on a `*str`) is `NoSuchField`, same as any other
    /// unrecognized name.
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

    /// `.ptr`/`.vtable` on a `spec *Spec` value -- not real fields (the
    /// concrete implementor is erased, so there's nothing to look up by
    /// name/index), answered before the aggregate paths below would reject
    /// it, exactly like `project_slice_field`. Both read one of the two
    /// leaves `ResolvedType::SpecObject`'s own doc comment describes; see
    /// `CheckedProjection::SpecObjectPtr`/`SpecObjectVtable` for why the
    /// pointee is always the opaque `u8` and which one tracks `mutable`.
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

    /// Enum member access: `tag`, header fields, and shared dynamic fields
    /// exist on every value; a body field additionally requires the value's
    /// variant to be statically known (see `ResolvedType::Enum`) *and* to be
    /// the one declaring it -- anything else gets the most precise "why not"
    /// this lookup can determine.
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
        // Read out of the cell in one borrow, released before any `&mut
        // self` call below.
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

    /// Everything reachable as `value.name` on an enum value, in lookup
    /// order: the tag (which has no declared visibility of its own), then
    /// the header, the shared dynamic fields, and finally the known
    /// variant's own body fields.
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

    /// Why `field` isn't reachable on this enum value: it belongs to another
    /// variant, it belongs to a variant this value's isn't known to be, or
    /// no variant declares it at all.
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
                // Suggest across everything reachable as `value.name` on this
                // value: tag, header, shared dynamic fields, and -- when the
                // variant is known -- its own body fields.
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

    /// A named field's position, type and declared visibility in one
    /// aggregate's own field list -- the shape struct/union fields, enum
    /// header fields, shared dynamic fields, and variant body fields all
    /// share.
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

    /// Rejects an access to a member the accessing code isn't allowed to see.
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

    /// Every method named `field` on `current_type` (after at most one
    /// pointer deref) -- usually zero or one, but two or more is a valid
    /// overload set (see `Analyzer::resolve_overload`, which the two call
    /// sites route a multi-candidate result through). A field with this
    /// name always shadows every same-named method, exactly like a single
    /// method would have.
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
                // Anything reachable as a field on *this* value (`tag`,
                // header, the known variant's body fields) shadows a
                // same-named function, matching the struct rule above.
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
            // Built-in types store their inherent methods in core's
            // `primitive` registry rather than in a declared type cell.
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

        // A conformance is deliberately not an inherent method source.
        // Instance syntax is admitted only while checking a body whose
        // generic/conform context established the matching bound.
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

    /// The name of an enum member that an assignment must not target --
    /// `Some` when `target`'s final projection reads the tag or a header
    /// field (both per-variant constants); see the `Assignment` arm of
    /// `analyze_expr`.
    pub(super) fn immutable_enum_member(target: &CheckedPlace) -> Option<Ident> {
        match target.projections.last()? {
            CheckedProjection::EnumTag { .. } => Some(Ident("tag".into())),
            CheckedProjection::EnumHeader { field, .. } => Some(field.clone()),
            _ => None,
        }
    }

    /// Hand-written structural equality between two checked places, used
    /// only to detect self-assignment (`x = x;`). Never attempts recursive
    /// sub-expression equality -- an `Expr` root, or an `Index`/`Deref`
    /// projection (whose "index" or "pointer" sub-expressions could
    /// themselves have side effects or simply differ at runtime), makes the
    /// comparison bail out as "not provably equal" rather than risk a false
    /// positive. A false negative here (missing a self-assignment) is fine;
    /// a false positive (warning on `a.foo = b.foo`) is not.
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

    /// Errors (returning `None`) unless a place `analyze_place` already
    /// resolved (`mutable` is its own third return value) may be written
    /// to. Shared by every requirement that ultimately means the same
    /// thing -- an assignment, `++`/`--`, an explicit `&mut`, and a `mut
    /// self` method call's implicit auto-ref are all, at bottom, "this
    /// place must be mutable" -- so the diagnostic (and the choice between
    /// `NotMutableBinding`/`NotMutablePointer`/`MutateTemporary`, mirroring
    /// `immutable_enum_member`'s pattern of inspecting the checked place's
    /// own projections) only needs writing once. `hir_root` is the
    /// *original* place's root, for naming the binding in
    /// `NotMutableBinding` -- only ever `None` when the reason is
    /// definitely `NotMutablePointer`/`MutateTemporary` instead (a
    /// non-place root, e.g. a freshly-constructed value: something
    /// dereferenced along the way, or nothing at all -- a spec-qualified
    /// call wraps a non-place receiver in `HirPlaceRoot::Expr` so it can be
    /// adapted at all, producing a place with a non-path root and *no*
    /// `Deref` projection, whose mutation would land in a discarded
    /// temporary).
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
            // This place is genuinely about to be written through -- the
            // one choke point every real reassignment (`=`, a compound
            // assignment, `++`/`--`, `&mut`) funnels through, so it's also
            // the one place `Context::mark_written` needs calling from.
            // Only a bare local variable root has a `decl_id` worth
            // tracking this way; a projection through a pointer/field/temp
            // isn't itself a *binding* `UnnecessaryMut` could ever fire on.
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
            // A receiver that is not a place at all: `Bump::bump(make())`.
            // The mutation would land in a freshly-produced value that is
            // immediately discarded -- no pointer is involved, and no added
            // `mut` can fix it, so `NotMutablePointer` (which names a
            // pointer that does not appear in the source) would be a lie.
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

    /// `&base[range]` (`requested_mutable: false`) / `&mut base[range]`
    /// (`requested_mutable: true`) -- the only way to produce a
    /// `ResolvedType::Slice` value; a bare `base[range]` with no `&`/`&mut`
    /// is rejected before this is ever called (see `HirExpr::Slice`'s arm
    /// in `analyze_expr`). Mirrors `HirExpr::AddressOf`'s own `&`/`&mut`
    /// treatment of an ordinary place, just producing a fat pointer instead
    /// of a thin one.
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
        // Snapshotted before the match below moves `base_type` -- only
        // needed by the `comp`-binding const-promotion path further down,
        // as the `Deref` projection's target type (see there for why).
        let base_type_snapshot = base_type.clone();
        // `*[?]T` (`Array`) -- itself just a pointer value with array-like
        // properties, see `ResolvedType::Array`'s own doc comment -- has no
        // length anywhere, at compile time or runtime, to default a
        // missing `end` to -- unlike `SizedArray` (its own compile-time
        // `N`) or `Slice`/`Str` (their own runtime length leaf). A plain
        // `Pointer` never reaches this match at all (see the base-type
        // match below), so it's not part of this check. Computed as a
        // plain `bool`, not re-derived from `base_type_snapshot` later,
        // since that gets moved out in one of the `comp`-binding branches
        // below.
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
            // `*[?]T` -- the mutability genuinely lives on the pointer
            // *value* (matching `Slice`/`Str`'s `true`, not `SizedArray`'s
            // binding-borne `false`), so `&mut an_immutable_arr[a..b]`
            // correctly blames the pointer itself (`ImmutableSliceSource`
            // below) rather than the binding holding it. A plain `*T`
            // pointer is *not* matched here -- `*T` is strictly a
            // single-value pointer, with no indexing or slicing of its
            // own; the only way to slice through one is to cast it to
            // `*[?]T` first (see `Context::resolve_pointer_type` and
            // `Analyzer::array_pointer_cast_kind`).
            ResolvedType::Array(item, mutable) => (*item, mutable, true, false),
            found => {
                self.error(node_id, span, AnalysisErrorKind::NotSliceable { found });
                return None;
            }
        };
        if requested_mutable && !source_mutable {
            // Re-slicing an already-immutable `Slice`/`Str` value: `require_
            // mutable_place` below would blame the *binding* (`&base.root`),
            // which is misleading here -- the binding may well be `mut`,
            // it's the fat-pointer *value* it holds that's immutable (see
            // the comment above). Only the plain-array case is a genuine
            // binding-mutability question.
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

        // `comp_binding[range]` -- const promotion, same as
        // `analyze_address_of`'s identical guard (see there, and
        // docs/19-compile-time-evaluation.md's "calling a method on a
        // `comp` binding" section). `requested_mutable` is already ruled
        // out by this point: a comp binding's own `source_mutable` is
        // always `false` (never `mut`), so the check just above already
        // rejected `&mut comp_binding[range]`.
        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = checked_base.root
        {
            let value = self.resolve_comp_place(node_id, span, &checked_base)?;
            checked_base = if from_fat_pointer {
                // Already its own fat pointer (`Slice`/`Str`) -- no address
                // needed at all, just materialize its two leaves (data
                // pointer + length) directly; whatever rodata blob its own
                // data pointer already targets (from whenever this comp
                // value was originally built) is reused as-is, no new
                // indirection layered on top.
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
                // Inline (`SizedArray`) storage -- needs a real address to
                // slice into, which a `comp` binding doesn't have; promote
                // it into one via the same `ConstValue::Ref` "address of a
                // separately-built piece of `comp` data" codegen already
                // knows how to emit into an anonymous rodata blob, then
                // dereference it like any other pointer -- identical
                // machinery to `analyze_address_of`'s own promotion, just
                // immediately deref'd back down since a slice needs the
                // *pointee*'s storage, not the pointer value itself.
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

    /// `&[...]` (compile-time slice literals are always immutable --
    /// `&mut [...]` is rejected here, unlike `analyze_slice`, which has a
    /// real mutable form). The element type comes from a declared/expected
    /// `Slice` type if one is in context (e.g. `x: *[i32] = &[1, 2, 3];`),
    /// otherwise from the first element's own ordinary-expression type
    /// (reusing `analyze_expr`'s existing literal-default inference, e.g.
    /// an unsuffixed number defaults to `i32`, rather than reinventing it)
    /// -- exactly the same two-source shape the ordinary `HirExpr::
    /// ArrayLiteral` arm above already uses. Every element is then
    /// re-evaluated as a compile-time constant via `const_eval_slice`, and
    /// the whole literal collapses to one `ConstValue::Slice`, baked into
    /// the binary's data segment at codegen (`Codegen::emit_const_slice`)
    /// rather than built on the stack.
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

    /// Resolves a place's root, then folds over its projections in source
    /// order, resolving field/index/deref projections against the running
    /// type and recording the exact resolved shape (field index, item/
    /// pointee type) so codegen never has to re-search or re-derive them.
    ///
    /// Also computes whether the *whole place* may be written to, in the
    /// same walk: it starts as the root's own mutability (a local/global
    /// binding's `VarBinding::mutable`; always `false` for anything reached
    /// through cross-module/qualified resolution, conservatively -- nothing
    /// in this language yet threads a real flag through `ResolvedItem`, and
    /// `false` is the safe default for "immutable unless proven otherwise"),
    /// and is *overwritten* (never combined) every time a `Deref` --
    /// explicit or the seamless one `resolve_field_projection` inserts for
    /// `ptr.field` -- or a `Slice` index is processed, by that pointer's/
    /// slice's own `mutable` flag: going through a pointer resets the
    /// mutability basis to that specific pointer's, regardless of what came
    /// before. A field access or an index into inline storage (`Array`/
    /// `SizedArray`, which aren't fat pointers) never changes it -- it
    /// simply inherits whatever the base's mutability already was.
    /// A place expression: a root (a local binding, an item, a
    /// type-qualified member, or a parenthesized expression) followed by
    /// zero or more `.field`/`[index]`/`*` projections.
    ///
    /// Hands back the checked place, what it resolves to, and whether it is
    /// writable -- mutability is a running property, reset by every pointer
    /// or slice hop along the way rather than decided once at the root.
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

    /// What a place starts from. Only a local binding can be writable at the
    /// root -- everything else needs a pointer or slice hop to become one.
    fn resolve_place_root(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        match &place.root {
            // A path with explicit generic arguments (`Optional<u32>::Some`,
            // `sum_generic<f64>`) -- resolved through the instantiating
            // machinery; see `resolve_generic_args_place`.
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
            // A qualified root -- either module-qualified (`mymodule::thing::
            // foo`, head an imported module alias) or type-qualified
            // (`MyStruct::do_thing`/`MyEnum::Variant`, head a type, the tail
            // one of its members). A module alias wins when both could apply,
            // preserving the module interpretation unchanged; a head that is
            // neither is reported by `resolve_type_qualified_value` with the
            // most precise error it can determine.
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
                    // A type-qualified member (`MyEnum::Variant`, a static
                    // function, ...) is never itself an assignable place --
                    // `mutable` is unconditionally `false` here, not just
                    // defaulted.
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

    /// An unqualified root: a local (function-body-level) binding wins if
    /// there is one; otherwise this is a same-module top-level reference,
    /// resolved exactly the way a qualified cross-module one is, with
    /// `module_path` supplying the implicit prefix.
    ///
    /// Values never need the indirect/in-progress distinction type
    /// resolution does -- only a named *type* can legitimately be
    /// mid-collection when referenced (see `ModuleResolver::resolve_item`) --
    /// so every item query from here passes `indirect = true`.
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

        // A bare, *uncalled* reference to a genuinely overloaded name --
        // claimed here before the alias path below can eagerly commit to one
        // arbitrary candidate (the same problem `resolve_bare_overload_
        // candidates` exists to avoid for a *call*).
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

    /// A bare reference to an overloaded name. Nothing about the name alone
    /// picks a candidate and there are no argument types to disambiguate
    /// with, so the only thing that can decide is an explicit function-typed
    /// `expected` -- exactly the situation `resolve_qualified_value` handles
    /// for the module-qualified shape.
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
        // No post-winner visibility check needed -- see
        // `resolve_overloaded_call`'s identical reasoning: `candidates` is
        // already the final, decided set (filtered, or fully admitted by
        // `import reveal`).
        let r#type = ResolvedType::Function(fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id,
            storage: Storage::Function,
            r#type: r#type.clone(),
        };
        Some((root, r#type))
    }

    /// `base[index]`. `Array` (the legacy thin-pointer unsized form, e.g.
    /// `argv`) and `SizedArray` are indexable inline storage, leaving
    /// mutability unchanged; a `Slice`/`Str` is a fat pointer whose own flag
    /// resets it, exactly like a deref. Codegen tells the three apart itself
    /// from this same type.
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
            // `SizedArray` is inline storage -- its writability is
            // whatever binding/field it's already stored in, so `mutable`
            // is deliberately left untouched here. `Array`, unlike
            // `SizedArray`, is a real pointer value with its own
            // type-level mutability (see `ResolvedType::Array`'s doc
            // comment) -- so it needs the same treatment `Slice`/`Str`
            // already get below, not the shared arm `SizedArray` gets.
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
            // Byte indexing, same as `*[u8]` -- symmetric with `Slice` above,
            // no artificial restriction (unlike Rust's `str`, which
            // disallows this entirely to avoid a byte/char-boundary footgun).
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
