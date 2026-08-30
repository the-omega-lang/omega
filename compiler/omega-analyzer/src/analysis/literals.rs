use super::*;

struct LiteralTargetFields {
    owner: Ident,
    declaring_module: Vec<Ident>,
    owner_id: HirId,
    base: ResolvedType,
    declared: Vec<ResolvedField>,
}

enum LiteralTarget {
    Struct(ResolvedType),
    EnumVariant(Rc<RefCell<ResolvedEnumType>>, usize),
    Union(ResolvedType),
}

pub(super) fn parse_number_literal(n: &NumberExpr, kind: NumericKind) -> Result<NumberValue, ()> {
    match kind {
        NumericKind::Float(width) => {
            let text = format!(
                "{}.{}",
                n.integer_part,
                n.fractional_part.as_deref().unwrap_or("0")
            );
            let parsed = text.parse::<f64>().map_err(|_| ())?;
            if width == 32 && parsed.is_finite() && (parsed as f32).is_infinite() {
                return Err(());
            }
            Ok(NumberValue::Float(parsed))
        }
        NumericKind::Signed(width) => {
            let parsed = u64::from_str_radix(&n.integer_part, n.base.radix()).map_err(|_| ())?;
            let max = if width == 64 {
                i64::MAX as u64
            } else {
                (1u64 << (width - 1)) - 1
            };
            if parsed > max {
                return Err(());
            }
            Ok(NumberValue::Signed(parsed as i64))
        }
        NumericKind::Unsigned(width) => {
            let parsed = u64::from_str_radix(&n.integer_part, n.base.radix()).map_err(|_| ())?;
            let max = if width == 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            if parsed > max {
                return Err(());
            }
            Ok(NumberValue::Unsigned(parsed))
        }
    }
}

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_struct_literal(
        &mut self,
        node_id: HirId,
        span: Span,
        lit: &HirStructLiteral,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        match self.resolve_literal_target(node_id, span, lit, expected)? {
            LiteralTarget::Struct(resolved) => {
                let ResolvedType::Struct(cell) = &resolved else {
                    unreachable!("LiteralTarget::Struct always wraps ResolvedType::Struct");
                };
                // Snapshot the declared fields so `cell` isn't borrowed
                // across the value analysis below -- a nested literal of
                // the same struct type needs to borrow it again.
                let declared = cell.borrow().fields.clone();
                let struct_name = cell.borrow().name.clone();
                let declaring_module = cell.borrow().module_path.clone();
                let owner_id = cell.borrow().id;
                let base = resolved.clone();
                let target = LiteralTargetFields {
                    owner: struct_name,
                    declaring_module,
                    owner_id,
                    base: base.clone(),
                    declared,
                };
                let fields =
                    self.check_field_initializers(node_id, span, &target, &lit.fields, |field| {
                        AnalysisErrorKind::NoSuchField {
                            field: field.name.clone(),
                            base: base.clone(),
                        }
                    })?;
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: resolved,
                    kind: CheckedExpr::StructLiteral(CheckedStructLiteral { fields }),
                })
            }
            LiteralTarget::EnumVariant(cell, variant_index) => {
                let (enum_name, variant_name, declared, header_names, declaring_module, owner_id) = {
                    let e = cell.borrow();
                    let v = &e.variants[variant_index];
                    let header_names: Vec<Ident> =
                        e.header.iter().map(|field| field.name.clone()).collect();
                    let declared: Vec<ResolvedField> = e
                        .dynamic_fields
                        .iter()
                        .chain(v.fields.iter())
                        .cloned()
                        .collect();
                    (
                        e.name.clone(),
                        v.name.clone(),
                        declared,
                        header_names,
                        e.module_path.clone(),
                        e.id,
                    )
                };
                if declared.is_empty() {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::EnumVariantHasNoBody {
                            r#enum: enum_name,
                            variant: variant_name,
                        },
                    );
                    return None;
                }
                let declared_names: Vec<Ident> =
                    declared.iter().map(|field| field.name.clone()).collect();
                let unknown_enum = enum_name.clone();
                let base = ResolvedType::Enum {
                    cell: cell.clone(),
                    variant: Some(variant_index),
                };
                let target = LiteralTargetFields {
                    owner: variant_name,
                    declaring_module,
                    owner_id,
                    base,
                    declared,
                };
                let fields = self.check_field_initializers(
                    node_id,
                    span,
                    &target,
                    &lit.fields,
                    move |field| {
                        if field.name.as_ref() == "tag" || header_names.contains(&field.name) {
                            AnalysisErrorKind::EnumHeaderFieldInLiteral {
                                field: field.name.clone(),
                            }
                        } else {
                            AnalysisErrorKind::NoSuchEnumField {
                                field: field.name.clone(),
                                r#enum: unknown_enum.clone(),
                                similar: best_match(&field.name, declared_names.iter()),
                            }
                        }
                    },
                )?;
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Enum {
                        cell,
                        variant: Some(variant_index),
                    },
                    kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct {
                        variant_index,
                        fields,
                    }),
                })
            }
            LiteralTarget::Union(resolved) => {
                let ResolvedType::Union(cell) = &resolved else {
                    unreachable!("LiteralTarget::Union always wraps ResolvedType::Union");
                };
                let declared = cell.borrow().fields.clone();
                let union_name = cell.borrow().name.clone();
                let declaring_module = cell.borrow().module_path.clone();
                let owner_id = cell.borrow().id;

                if lit.fields.is_empty() {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnionLiteralMissingField {
                            r#union: union_name,
                        },
                    );
                    return None;
                }
                if lit.fields.len() > 1 {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnionLiteralTooManyFields {
                            r#union: union_name,
                            fields: lit.fields.iter().map(|f| f.name.clone()).collect(),
                        },
                    );
                    return None;
                }

                let field = &lit.fields[0];
                let found = declared
                    .iter()
                    .enumerate()
                    .find(|(_, declared)| declared.name == field.name)
                    .map(|(index, declared)| (index, declared.r#type.clone(), declared.visibility));
                let Some((field_index, expected, visibility)) = found else {
                    self.error(
                        node_id,
                        field.name_span,
                        AnalysisErrorKind::NoSuchField {
                            field: field.name.clone(),
                            base: resolved.clone(),
                        },
                    );
                    return None;
                };
                if !self.check_member_visibility(
                    visibility,
                    &declaring_module,
                    owner_id,
                    field.name_origin,
                ) {
                    self.error(
                        node_id,
                        field.name_span,
                        AnalysisErrorKind::FieldNotVisible {
                            field: field.name.clone(),
                            base: resolved.clone(),
                        },
                    );
                    return None;
                }
                let value = self.analyze_expr(&field.value, Some(&expected))?;
                if !expected.accepts(&value.r#type) {
                    self.error(
                        node_id,
                        value.span,
                        AnalysisErrorKind::FieldTypeMismatch {
                            field: field.name.clone(),
                            expected,
                            found: value.r#type.clone(),
                        },
                    );
                    return None;
                }

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: resolved,
                    kind: CheckedExpr::UnionConstruct(CheckedUnionConstruct {
                        field_index,
                        value: Box::new(value),
                    }),
                })
            }
        }
    }

    fn check_field_initializers(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &LiteralTargetFields,
        fields: &[omega_hir::HirStructLiteralField],
        unknown_field: impl Fn(&omega_hir::HirStructLiteralField) -> AnalysisErrorKind,
    ) -> Option<Vec<CheckedStructLiteralField>> {
        let LiteralTargetFields {
            owner,
            declaring_module,
            owner_id,
            base,
            declared,
        } = target;
        let owner_id = *owner_id;
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        let mut checked_fields = Vec::with_capacity(fields.len());
        let mut ok = true;
        for field in fields {
            if let Some(previous) = seen.insert(field.name.clone(), field.name_span) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::DuplicateFieldInitializer {
                        field: field.name.clone(),
                        previous,
                    },
                );
                ok = false;
                continue;
            }
            let found = declared
                .iter()
                .enumerate()
                .find(|(_, declared)| declared.name == field.name)
                .map(|(index, declared)| (index, declared.r#type.clone(), declared.visibility));
            let Some((field_index, expected, visibility)) = found else {
                self.error(node_id, field.name_span, unknown_field(field));
                ok = false;
                continue;
            };
            if !self.check_member_visibility(
                visibility,
                declaring_module,
                owner_id,
                field.name_origin,
            ) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::FieldNotVisible {
                        field: field.name.clone(),
                        base: base.clone(),
                    },
                );
                ok = false;
                continue;
            }
            let Some(value) = self
                .analyze_expr(&field.value, Some(&expected))
                .map(|value| self.coerce_to_expected(Some(&expected), value))
            else {
                ok = false;
                continue;
            };
            if !expected.accepts(&value.r#type) {
                self.error(
                    node_id,
                    value.span,
                    AnalysisErrorKind::FieldTypeMismatch {
                        field: field.name.clone(),
                        expected,
                        found: value.r#type.clone(),
                    },
                );
                ok = false;
                continue;
            }
            checked_fields.push(CheckedStructLiteralField { field_index, value });
        }

        let missing: Vec<Ident> = declared
            .iter()
            .map(|field| &field.name)
            .filter(|name| !seen.contains_key(*name))
            .cloned()
            .collect();
        if !missing.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MissingFieldInitializers {
                    r#struct: owner.clone(),
                    missing,
                },
            );
            ok = false;
        }

        ok.then_some(checked_fields)
    }

    fn resolve_literal_target(
        &mut self,
        node_id: HirId,
        span: Span,
        lit: &HirStructLiteral,
        expected: Option<&ResolvedType>,
    ) -> Option<LiteralTarget> {
        let path = &lit.path;
        if path.plain().is_none() {
            let segments = path.path.segments();
            let rest = segments[path.args_at + 1..].to_vec();
            if rest.len() > 1 {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::GenericPathTooDeep {
                        r#type: segments[path.args_at].clone(),
                    },
                );
                return None;
            }
            let prefix = &segments[..=path.args_at];
            let access = self.generic_prefix_absolute(node_id, span, &path.path, prefix)?;
            let absolute = access.absolute.clone();
            let accessor = self.path_module(&path.path);
            let params = self.item_generic_params_for(&accessor, prefix, &access);
            let generic_args = self.resolve_generic_arg_list(
                node_id,
                span,
                &path.generic_args,
                &access.absolute,
                &params,
            )?;
            let resolved = match self.resolve_item_with_ambient_from(
                &accessor,
                prefix,
                &access,
                &generic_args,
            ) {
                Ok(ResolvedItem::Type(t)) => t,
                Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(
                            crate::error::TypeResolutionError::NotAType(absolute),
                        ),
                    );
                    return None;
                }
                Err(e) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                    return None;
                }
            };
            return self.literal_target_from_type(node_id, span, resolved, &rest);
        }

        let plain = &path.path;

        if plain.is_unqualified() {
            if let Some(local) = self.context.find_defined_type(&plain.head).cloned() {
                return self.literal_target_from_type(node_id, span, local, &[]);
            }
            let alias = self.resolve_alias_or_error(node_id, span, &plain.head)?;
            let access = match &alias {
                Some(ImportTarget::Item(absolute, _)) | Some(ImportTarget::Module(absolute)) => {
                    ItemAccess::gated(absolute.clone())
                }
                Some(ImportTarget::ItemPath(access)) => access.clone(),
                None => ItemAccess::gated(
                    self.module_path
                        .iter()
                        .cloned()
                        .chain(std::iter::once(plain.head.clone()))
                        .collect(),
                ),
            };
            let absolute = access.absolute.clone();
            if let Some((real_absolute, sig)) = self.generic_literal_signature_with_ambient(
                std::slice::from_ref(&plain.head),
                &absolute,
                None,
            ) {
                let result = self.resolve_generic_literal(
                    node_id,
                    span,
                    std::slice::from_ref(&plain.head),
                    &ItemAccess {
                        bypass_visibility: access.bypass_visibility && real_absolute == absolute,
                        absolute: real_absolute.clone(),
                    },
                    &sig,
                    &lit.fields,
                    expected,
                    plain.origin,
                )?;
                let resolved = match result {
                    Ok(ResolvedItem::Type(t)) => t,
                    Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotAType(
                                real_absolute,
                            )),
                        );
                        return None;
                    }
                    Err(e) => {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::UnresolvedType(
                                TypeResolutionError::ModuleResolution(e),
                            ),
                        );
                        return None;
                    }
                };
                return self.literal_target_from_type(node_id, span, resolved, &[]);
            }
            let resolved =
                self.resolve_type_or_error(node_id, span, &Type::Named(plain.clone()), true)?;
            return self.literal_target_from_type(node_id, span, resolved, &[]);
        }

        // Anchored paths may still be type-qualified (`self::Type::Variant`),
        // so preserve the old whole-path/prefix fallback when the path is not
        // a module binding. When it *is* module-qualified, canonicalize every
        // module-alias segment first.
        let anchored = match self.anchored_path(node_id, span, plain) {
            AnchoredPath::Failed => return None,
            AnchoredPath::Absolute(absolute) => Some(absolute),
            AnchoredPath::Unanchored => None,
        };
        let module_qualified = match self.module_qualified_path(node_id, span, plain) {
            ModuleQualifiedPath::Item(access) => Some(access),
            ModuleQualifiedPath::NotModule => anchored
                .as_ref()
                .map(|absolute| ItemAccess::gated(absolute.clone())),
            ModuleQualifiedPath::Failed => return None,
        };
        let alias = if anchored.is_some() || module_qualified.is_some() {
            None
        } else {
            self.resolve_alias_or_error(node_id, span, &plain.head)?
        };
        if let Some(access) = module_qualified {
            let absolute = access.absolute.clone();
            let whole_result = match self.resolver.generic_literal_signature(&absolute, None) {
                Ok(Some(sig)) => self.resolve_generic_literal(
                    node_id,
                    span,
                    &absolute,
                    &access,
                    &sig,
                    &lit.fields,
                    expected,
                    plain.origin,
                )?,
                _ => self.resolve_item_checked(&access, &[], true, plain.origin),
            };
            let first_error = match whole_result {
                Ok(ResolvedItem::Type(t)) => {
                    return self.literal_target_from_type(node_id, span, t, &[]);
                }
                Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(
                            crate::error::TypeResolutionError::NotAType(absolute),
                        ),
                    );
                    return None;
                }
                Err(e) => e,
            };
            if absolute.len() >= 3 {
                let (variant, prefix) = absolute.split_last().expect("length checked above");
                let accessor = self.path_module(plain);
                let prefix_access = self.canonicalize_item_access(
                    node_id,
                    span,
                    &accessor,
                    ItemAccess::gated(prefix.to_vec()),
                )?;
                let variant_result = match self
                    .resolver
                    .generic_literal_signature(&prefix_access.absolute, Some(variant))
                {
                    Ok(Some(sig)) => self.resolve_generic_literal(
                        node_id,
                        span,
                        &prefix_access.absolute,
                        &prefix_access,
                        &sig,
                        &lit.fields,
                        expected,
                        plain.origin,
                    )?,
                    _ => self.resolve_item_checked(&prefix_access, &[], true, plain.origin),
                };
                if let Ok(ResolvedItem::Type(t)) = variant_result {
                    return self.literal_target_from_type(
                        node_id,
                        span,
                        t,
                        std::slice::from_ref(variant),
                    );
                }
            }
            self.error(
                node_id,
                span,
                AnalysisErrorKind::ModuleResolution(first_error),
            );
            return None;
        }

        if let Some(head_type) = self.context.find_defined_type(&plain.head).cloned() {
            return self.literal_target_from_type(node_id, span, head_type, &plain.tail);
        }
        if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
            return self.literal_target_from_type(node_id, span, t, &plain.tail);
        }
        let access = match alias {
            Some(ImportTarget::ItemPath(access)) => access,
            _ => ItemAccess::gated(
                self.module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(plain.head.clone()))
                    .collect(),
            ),
        };
        let absolute = access.absolute.clone();
        let variant = (plain.tail.len() == 1).then(|| &plain.tail[0]);
        let result = match self.generic_literal_signature_with_ambient(
            std::slice::from_ref(&plain.head),
            &absolute,
            variant,
        ) {
            Some((real_absolute, sig)) => self.resolve_generic_literal(
                node_id,
                span,
                std::slice::from_ref(&plain.head),
                &ItemAccess {
                    bypass_visibility: access.bypass_visibility && real_absolute == absolute,
                    absolute: real_absolute,
                },
                &sig,
                &lit.fields,
                expected,
                plain.origin,
            )?,
            None => self.resolve_item_checked_with_ambient_fallback(
                std::slice::from_ref(&plain.head),
                &access,
                &[],
                plain.origin,
            ),
        };
        let kind = match result {
            Ok(ResolvedItem::Type(t)) => {
                return self.literal_target_from_type(node_id, span, t, &plain.tail);
            }
            Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                AnalysisErrorKind::NotAModule {
                    name: plain.head.clone(),
                }
            }
            Err(ResolveError::UnknownItem { .. }) => AnalysisErrorKind::UndefinedPathHead {
                name: plain.head.clone(),
                similar_module: self.similar_import_alias(&plain.head),
                similar_type: self.context.similar_type_name(&plain.head).or_else(|| {
                    self.resolver.similar_item_name(
                        &self.module_path,
                        &plain.head,
                        ItemNamespace::Type,
                    )
                }),
            },
            Err(e) => AnalysisErrorKind::ModuleResolution(e),
        };
        self.error(node_id, span, kind);
        None
    }

    pub(super) fn generic_literal_signature_with_ambient(
        &mut self,
        prefix: &[Ident],
        absolute: &[Ident],
        variant: Option<&Ident>,
    ) -> Option<(Vec<Ident>, GenericLiteralSignature)> {
        if let Ok(Some(sig)) = self.resolver.generic_literal_signature(absolute, variant) {
            return Some((absolute.to_vec(), sig));
        }
        let [single] = prefix else { return None };
        let ambient = self
            .resolver
            .ambient_core_candidates(&self.module_path, single)
            .ok()
            .flatten()?;
        let sig = self
            .resolver
            .generic_literal_signature(&ambient, variant)
            .ok()
            .flatten()?;
        Some((ambient, sig))
    }

    fn resolve_generic_literal(
        &mut self,
        node_id: HirId,
        span: Span,
        prefix: &[Ident],
        access: &ItemAccess,
        sig: &GenericLiteralSignature,
        lit_fields: &[HirStructLiteralField],
        expected: Option<&ResolvedType>,
        origin: Origin,
    ) -> Option<Result<ResolvedItem, ResolveError>> {
        let generic_args = self.infer_literal_type_args(
            node_id,
            span,
            &access.absolute,
            sig,
            lit_fields,
            expected,
        )?;
        Some(self.resolve_item_checked_with_ambient_fallback(prefix, access, &generic_args, origin))
    }

    pub(super) fn infer_literal_type_args(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: &[Ident],
        sig: &GenericLiteralSignature,
        lit_fields: &[HirStructLiteralField],
        expected: Option<&ResolvedType>,
    ) -> Option<Vec<ResolvedGenericArg>> {
        if let Some(generic_args) = Self::expected_matches_generic_item(expected, absolute) {
            return Some(generic_args);
        }
        let comp_types =
            self.comp_param_types(node_id, span, &sig.generics, &GenericSubstitution::new());
        let generics = self.generic_params(&sig.generics, &comp_types);
        let subst = self.probe_literal_generic_args(sig, &generics, lit_fields)?;
        match resolve_inferred_generic_args(&generics, &subst) {
            Ok(generic_args) => Some(generic_args),
            Err(_) => {
                let missing: Vec<Ident> = sig
                    .generics
                    .iter()
                    .filter(|param| param.default.is_none() && !subst.contains(&param.ident))
                    .map(|param| param.ident.clone())
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UnresolvedLiteralGeneric {
                        r#type: absolute
                            .last()
                            .cloned()
                            .expect("an absolute path always has a last segment"),
                        generics: missing,
                    },
                );
                None
            }
        }
    }

    fn expected_matches_generic_item(
        expected: Option<&ResolvedType>,
        absolute: &[Ident],
    ) -> Option<Vec<ResolvedGenericArg>> {
        let expected = expected?;
        let (name, module) = absolute.split_last()?;
        let (cell_module, cell_name, generic_args) = match expected {
            ResolvedType::Struct(cell) => {
                let c = cell.borrow();
                (
                    c.module_path.clone(),
                    c.name.clone(),
                    c.generic_args.clone(),
                )
            }
            ResolvedType::Union(cell) => {
                let c = cell.borrow();
                (
                    c.module_path.clone(),
                    c.name.clone(),
                    c.generic_args.clone(),
                )
            }
            ResolvedType::Enum { cell, .. } => {
                let c = cell.borrow();
                (
                    c.module_path.clone(),
                    c.name.clone(),
                    c.generic_args.clone(),
                )
            }
            _ => return None,
        };
        (cell_module == module && &cell_name == name).then_some(generic_args)
    }

    fn probe_literal_generic_args(
        &mut self,
        sig: &GenericLiteralSignature,
        generics: &GenericParams<'_>,
        lit_fields: &[HirStructLiteralField],
    ) -> Option<GenericSubstitution> {
        let errors_before = self.errors.len();
        let warnings_before = self.warnings.len();
        let mut subst = GenericSubstitution::new();
        let mut ok = true;
        for field in lit_fields {
            let Some((_, raw_type)) = sig.fields.iter().find(|(name, _)| name == &field.name)
            else {
                continue;
            };
            let expected = self.expected_for_generic_param(
                field.value.id,
                field.value.span,
                raw_type,
                generics,
                &subst,
            );
            match self.analyze_expr(&field.value, expected.as_ref()) {
                Some(checked) => {
                    unify_generic_type(generics, raw_type, &checked.r#type, &mut subst)
                }
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        self.errors.truncate(errors_before);
        self.warnings.truncate(warnings_before);
        Some(subst)
    }

    fn literal_target_from_type(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: ResolvedType,
        rest: &[Ident],
    ) -> Option<LiteralTarget> {
        let kind = match &r#type {
            ResolvedType::Struct(cell) => match rest.first() {
                None => return Some(LiteralTarget::Struct(r#type.clone())),
                Some(name) => AnalysisErrorKind::StructLiteralPathTooDeep {
                    r#struct: cell.borrow().name.clone(),
                    name: name.clone(),
                },
            },
            ResolvedType::Union(cell) => match rest.first() {
                None => return Some(LiteralTarget::Union(r#type.clone())),
                Some(name) => AnalysisErrorKind::StructLiteralPathTooDeep {
                    r#struct: cell.borrow().name.clone(),
                    name: name.clone(),
                },
            },
            ResolvedType::Enum { cell, .. } => match rest {
                [] => {
                    let e = cell.borrow();
                    AnalysisErrorKind::EnumLiteralWithoutVariant {
                        r#enum: e.name.clone(),
                        example: e
                            .variants
                            .first()
                            .map(|v| v.name.clone())
                            .unwrap_or_else(|| Ident("Variant".into())),
                    }
                }
                [variant_name] => {
                    let found = cell.borrow().variant(variant_name).map(|(index, _)| index);
                    match found {
                        Some(index) => {
                            return Some(LiteralTarget::EnumVariant(cell.clone(), index));
                        }
                        None => {
                            let e = cell.borrow();
                            AnalysisErrorKind::NoSuchEnumMember {
                                r#enum: e.name.clone(),
                                name: variant_name.clone(),
                                similar_variant: best_match(
                                    variant_name,
                                    e.variants.iter().map(|v| &v.name),
                                ),
                                // An enum literal names a variant, so only
                                // the namespace variants share can offer a
                                // near miss here.
                                similar_function: best_match(
                                    variant_name,
                                    FunctionNamespace::Static.names(&e.functions).into_iter(),
                                ),
                            }
                        }
                    }
                }
                _ => AnalysisErrorKind::GenericPathTooDeep {
                    r#type: cell.borrow().name.clone(),
                },
            },
            _ if rest.is_empty() => AnalysisErrorKind::StructLiteralNotAStruct {
                found: r#type.clone(),
            },
            _ => AnalysisErrorKind::StaticAccessOnNonStruct {
                found: r#type.clone(),
            },
        };
        self.error(node_id, span, kind);
        None
    }

    pub(super) fn analyze_number(
        &mut self,
        node_id: HirId,
        span: Span,
        n: &NumberExpr,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let invalid_suffix = |this: &mut Self, ident: &Ident| {
            this.error(
                node_id,
                span,
                AnalysisErrorKind::InvalidNumberType(ident.clone()),
            );
        };

        let reveals = &self.reveals;
        let resolved_type = match &n.explicit_type {
            Some(explicit_type) => match self.context.resolve_type(
                Type::Named(explicit_type.clone().into()),
                &mut *self.resolver,
                &self.module_path,
                ResolveItemOptions::INDIRECT,
                &|origin| reveals.allows(origin),
            ) {
                Ok(r#type) if r#type.numeric_kind(self.target.pointer_bits()).is_some() => r#type,
                _ => {
                    invalid_suffix(self, explicit_type);
                    return None;
                }
            },
            None => Self::default_or_expected_number_type(n, expected, self.target.pointer_bits()),
        };
        let kind = resolved_type
            .numeric_kind(self.target.pointer_bits())
            .expect("just resolved above, or a hardcoded numeric default");

        let is_float = matches!(kind, NumericKind::Float(_));
        if n.fractional_part.is_some() && !is_float {
            let Some(explicit_type) = &n.explicit_type else {
                unreachable!("the default type for a fractional literal is always F64");
            };
            invalid_suffix(self, explicit_type);
            return None;
        }
        if is_float && n.base != NumberBase::Decimal {
            let Some(explicit_type) = &n.explicit_type else {
                unreachable!(
                    "the default type is only Float when a fraction was written, which implies Decimal"
                );
            };
            invalid_suffix(self, explicit_type);
            return None;
        }

        let Ok(value) = parse_number_literal(n, kind) else {
            let literal_text = match &n.fractional_part {
                Some(frac) => format!("{}.{}", n.integer_part, frac),
                None => n.integer_part.clone(),
            };
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NumberLiteralOutOfRange {
                    literal: literal_text,
                    r#type: resolved_type,
                },
            );
            return None;
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: resolved_type,
            kind: CheckedExpr::Number(value),
        })
    }

    fn default_or_expected_number_type(
        n: &NumberExpr,
        expected: Option<&ResolvedType>,
        pointer_bits: u32,
    ) -> ResolvedType {
        let default = if n.fractional_part.is_some() {
            ResolvedType::F32
        } else {
            ResolvedType::I32
        };
        let Some(expected) = expected else {
            return default;
        };
        let Some(kind) = expected.numeric_kind(pointer_bits) else {
            return default;
        };
        if matches!(kind, NumericKind::Float(_)) == n.fractional_part.is_some() {
            expected.clone()
        } else {
            default
        }
    }

    pub(super) fn adaptable_literal(expr: &HirExprNode) -> bool {
        match &expr.expr {
            HirExpr::Number(n) => n.explicit_type.is_none(),
            HirExpr::Negate(inner) => {
                matches!(&inner.expr, HirExpr::Number(n) if n.explicit_type.is_none())
            }
            _ => false,
        }
    }

    pub(super) fn analyze_array_literal(
        &mut self,
        node_id: HirId,
        span: Span,
        elements: &[HirExprNode],
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let Some((first, rest)) = elements.split_first() else {
            self.error(node_id, span, AnalysisErrorKind::EmptyArrayLiteral);
            return None;
        };

        // A declared/expected element type (from `[T; N]` context) is used
        // as every element's own expected type, including the first --
        // unlike the bottom-up fallback below, where later elements are
        // checked against the first's inferred type.
        let declared_item_type = match expected {
            Some(ResolvedType::SizedArray(item_type, _)) => Some(item_type.as_ref()),
            _ => None,
        };

        let checked_first = self
            .analyze_expr(first, declared_item_type)
            .map(|value| self.coerce_to_expected(declared_item_type, value))?;
        let item_type = declared_item_type
            .cloned()
            .unwrap_or_else(|| checked_first.r#type.widened());

        let mut checked_elements = Vec::with_capacity(elements.len());
        let check_element =
            |this: &mut Self, id: HirId, elem_span: Span, checked: CheckedExprNode| {
                if !item_type.accepts(&checked.r#type) {
                    this.error(
                        id,
                        elem_span,
                        AnalysisErrorKind::ArrayElementTypeMismatch {
                            expected: item_type.clone(),
                            found: checked.r#type.clone(),
                        },
                    );
                    return None;
                }
                Some(checked)
            };
        checked_elements.push(check_element(self, first.id, first.span, checked_first)?);

        for element in rest {
            let checked_element = self
                .analyze_expr(element, Some(&item_type))
                .map(|value| self.coerce_to_expected(Some(&item_type), value))?;
            checked_elements.push(check_element(
                self,
                element.id,
                element.span,
                checked_element,
            )?);
        }

        let size = checked_elements.len() as u32;
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::SizedArray(Box::new(item_type.clone()), size),
            kind: CheckedExpr::ArrayLiteral(CheckedArrayLiteral {
                item_type,
                elements: checked_elements,
            }),
        })
    }
}
