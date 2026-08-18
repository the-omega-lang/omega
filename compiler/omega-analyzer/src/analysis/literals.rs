use super::*;

/// The declaration a `Name { ... }` literal initializes, reduced to what
/// checking its field initializers needs -- identical whether it came from a
/// struct, a union, or one enum variant.
struct LiteralTargetFields {
    /// The name a "missing field" error shows: a struct's or union's own
    /// name, or the variant's.
    owner: Ident,
    declaring_module: Vec<Ident>,
    owner_id: HirId,
    /// The literal's own resolved type, for a field-visibility error.
    base: ResolvedType,
    declared: Vec<(Ident, ResolvedType, Visibility)>,
}

/// What a `Name { ... }` literal's path resolved to -- see
/// `Analyzer::resolve_literal_target`.
enum LiteralTarget {
    /// Always wraps `ResolvedType::Struct`.
    Struct(ResolvedType),
    EnumVariant(Rc<RefCell<ResolvedEnumType>>, usize),
    /// Always wraps `ResolvedType::Union`.
    Union(ResolvedType),
}

/// The pure parse-and-range-check core behind a number literal's concrete
/// value -- no `Span`/error-pushing, just `Err(())` on failure, so this is
/// usable both from `Analyzer::analyze_number`'s error-reporting path and
/// from overload-viability scoring's silent "would this literal fit this
/// candidate" check. `kind` is whatever concrete numeric type the caller
/// already decided on; this never picks the type itself, only validates the
/// literal's digits against it.
pub(super) fn parse_number_literal(n: &NumberExpr, kind: NumericKind) -> Result<NumberValue, ()> {
    match kind {
        NumericKind::Float(width) => {
            let text = format!("{}.{}", n.integer_part, n.fractional_part.as_deref().unwrap_or("0"));
            let parsed = text.parse::<f64>().map_err(|_| ())?;
            if width == 32 && parsed.is_finite() && (parsed as f32).is_infinite() {
                return Err(());
            }
            Ok(NumberValue::Float(parsed))
        }
        NumericKind::Signed(width) => {
            let parsed = u64::from_str_radix(&n.integer_part, n.base.radix()).map_err(|_| ())?;
            let max = if width == 64 { i64::MAX as u64 } else { (1u64 << (width - 1)) - 1 };
            if parsed > max {
                return Err(());
            }
            Ok(NumberValue::Signed(parsed as i64))
        }
        NumericKind::Unsigned(width) => {
            let parsed = u64::from_str_radix(&n.integer_part, n.base.radix()).map_err(|_| ())?;
            let max = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
            if parsed > max {
                return Err(());
            }
            Ok(NumberValue::Unsigned(parsed))
        }
    }
}

impl<'r> Analyzer<'r> {
    /// `Name { field = value; ... }` -- builds a whole struct value, or --
    /// when the path names an enum variant -- a whole enum value. Every
    /// declared field must be set exactly once with a value of its exact
    /// type. All field problems in one literal are reported in one pass,
    /// not just the first.
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
                let declared: Vec<(Ident, ResolvedType, Visibility)> = cell.borrow().fields.clone();
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
                let fields = self.check_field_initializers(node_id, span, &target, &lit.fields, |field| {
                    AnalysisErrorKind::NoSuchField { field: field.name.clone(), base: base.clone() }
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
                    let header_names: Vec<Ident> = e.header.iter().map(|(name, _, _)| name.clone()).collect();
                    // Shared dynamic fields first (declaration order), then
                    // this variant's own body fields -- every construction
                    // site must supply both, in one combined literal.
                    let declared: Vec<(Ident, ResolvedType, Visibility)> =
                        e.dynamic_fields.iter().chain(v.fields.iter()).cloned().collect();
                    (e.name.clone(), v.name.clone(), declared, header_names, e.module_path.clone(), e.id)
                };
                if declared.is_empty() {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::EnumVariantHasNoBody { r#enum: enum_name, variant: variant_name },
                    );
                    return None;
                }
                let declared_names: Vec<Ident> = declared.iter().map(|(name, _, _)| name.clone()).collect();
                let unknown_enum = enum_name.clone();
                let base = ResolvedType::Enum { cell: cell.clone(), variant: Some(variant_index) };
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
                            AnalysisErrorKind::EnumHeaderFieldInLiteral { field: field.name.clone() }
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
                    r#type: ResolvedType::Enum { cell, variant: Some(variant_index) },
                    kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct { variant_index, fields }),
                })
            }
            LiteralTarget::Union(resolved) => {
                let ResolvedType::Union(cell) = &resolved else {
                    unreachable!("LiteralTarget::Union always wraps ResolvedType::Union");
                };
                let declared: Vec<(Ident, ResolvedType, Visibility)> = cell.borrow().fields.clone();
                let union_name = cell.borrow().name.clone();
                let declaring_module = cell.borrow().module_path.clone();
                let owner_id = cell.borrow().id;

                if lit.fields.is_empty() {
                    self.error(node_id, span, AnalysisErrorKind::UnionLiteralMissingField { r#union: union_name });
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
                    .find(|(_, (name, _, _))| name == &field.name)
                    .map(|(index, (_, r#type, visibility))| (index, r#type.clone(), *visibility));
                let Some((field_index, expected, visibility)) = found else {
                    self.error(
                        node_id,
                        field.name_span,
                        AnalysisErrorKind::NoSuchField { field: field.name.clone(), base: resolved.clone() },
                    );
                    return None;
                };
                if !self.check_member_visibility(visibility, &declaring_module, owner_id) {
                    self.error(
                        node_id,
                        field.name_span,
                        AnalysisErrorKind::FieldNotVisible { field: field.name.clone(), base: resolved.clone() },
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
                    kind: CheckedExpr::UnionConstruct(CheckedUnionConstruct { field_index, value: Box::new(value) }),
                })
            }
        }
    }

    /// The shared per-field discipline behind both literal forms: each
    /// initializer must name a declared field, exactly once, with a value
    /// of its field's type; every declared field must be covered (there is
    /// no implicit zeroing). `unknown_field` supplies the form-specific "no
    /// such field" diagnostic.
    fn check_field_initializers(
        &mut self,
        node_id: HirId,
        span: Span,
        target: &LiteralTargetFields,
        fields: &[omega_hir::HirStructLiteralField],
        unknown_field: impl Fn(&omega_hir::HirStructLiteralField) -> AnalysisErrorKind,
    ) -> Option<Vec<CheckedStructLiteralField>> {
        let LiteralTargetFields { owner, declaring_module, owner_id, base, declared } = target;
        let owner_id = *owner_id;
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        let mut checked_fields = Vec::with_capacity(fields.len());
        let mut ok = true;
        for field in fields {
            if let Some(previous) = seen.insert(field.name.clone(), field.name_span) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::DuplicateFieldInitializer { field: field.name.clone(), previous },
                );
                ok = false;
                continue;
            }
            let found = declared
                .iter()
                .enumerate()
                .find(|(_, (name, _, _))| name == &field.name)
                .map(|(index, (_, r#type, visibility))| (index, r#type.clone(), *visibility));
            let Some((field_index, expected, visibility)) = found else {
                self.error(node_id, field.name_span, unknown_field(field));
                ok = false;
                continue;
            };
            if !self.check_member_visibility(visibility, declaring_module, owner_id) {
                self.error(
                    node_id,
                    field.name_span,
                    AnalysisErrorKind::FieldNotVisible { field: field.name.clone(), base: base.clone() },
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

        let missing: Vec<Ident> =
            declared.iter().map(|(name, _, _)| name).filter(|name| !seen.contains_key(name)).cloned().collect();
        if !missing.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MissingFieldInitializers { r#struct: owner.clone(), missing },
            );
            ok = false;
        }

        ok.then_some(checked_fields)
    }

    /// What a `Name { ... }` literal's path actually names -- a struct, or
    /// one specific variant of an enum. Resolution order mirrors place-root
    /// resolution: explicit generic arguments pin the type prefix exactly;
    /// otherwise an imported-module alias reading of the head wins (whole
    /// path as the type first, then all-but-last as an enum with the last
    /// segment its variant), and a non-alias multi-segment head must itself
    /// be a type in scope or this module's own.
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
                    AnalysisErrorKind::GenericPathTooDeep { r#type: segments[path.args_at].clone() },
                );
                return None;
            }
            let type_args = self.resolve_generic_arg_list(node_id, span, path)?;
            let prefix = &segments[..=path.args_at];
            let absolute = self.generic_prefix_absolute(node_id, span, &path.path, prefix)?;
            let accessor = self.path_module(&path.path);
            let resolved = match self.resolve_item_with_ambient_from(&accessor, prefix, &absolute, &type_args) {
                Ok(ResolvedItem::Type(t)) => t,
                Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(crate::error::TypeResolutionError::NotAType(absolute)),
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

        // A bare name: a written type annotation, unless it names a
        // *generic* struct/union, in which case its omitted type arguments
        // are inferred first (see `infer_literal_type_args`).
        if plain.is_unqualified() {
            if let Some(local) = self.context.find_defined_type(&plain.head).cloned() {
                return self.literal_target_from_type(node_id, span, local, &[]);
            }
            let alias = self.resolve_alias_or_error(node_id, span, &plain.head)?;
            let absolute: Vec<Ident> = match &alias {
                Some(ImportTarget::Item(absolute, _))
                | Some(ImportTarget::GenericItem(absolute))
                | Some(ImportTarget::Module(absolute)) => absolute.clone(),
                None => self.module_path.iter().cloned().chain(std::iter::once(plain.head.clone())).collect(),
            };
            if let Some((real_absolute, sig)) =
                self.generic_literal_signature_with_ambient(std::slice::from_ref(&plain.head), &absolute, None)
            {
                let result = self.resolve_generic_literal(
                    node_id,
                    span,
                    std::slice::from_ref(&plain.head),
                    &real_absolute,
                    &sig,
                    &lit.fields,
                    expected,
                )?;
                let resolved = match result {
                    Ok(ResolvedItem::Type(t)) => t,
                    Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                        self.error(node_id, span, AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotAType(real_absolute)));
                        return None;
                    }
                    Err(e) => {
                        self.error(node_id, span, AnalysisErrorKind::UnresolvedType(TypeResolutionError::ModuleResolution(e)));
                        return None;
                    }
                };
                return self.literal_target_from_type(node_id, span, resolved, &[]);
            }
            let resolved = self.resolve_type_or_error(node_id, span, &Type::Named(plain.clone()), true)?;
            return self.literal_target_from_type(node_id, span, resolved, &[]);
        }

        // Module-qualified head: the whole path as the type first
        // (`mymodule::Vec2 { ... }`), then all-but-last as an enum whose
        // last segment names the variant (`mymodule::Shape::Circle`) --
        // each attempt tries generic inference first (a no-op for the
        // common non-generic case).
        let alias = self.resolve_alias_or_error(node_id, span, &plain.head)?;
        if let Some(ImportTarget::Module(target)) = &alias {
            let absolute: Vec<Ident> = target.iter().cloned().chain(plain.tail.iter().cloned()).collect();
            let whole_result = match self.resolver.generic_literal_signature(&absolute, None) {
                Ok(Some(sig)) => {
                    self.resolve_generic_literal(node_id, span, &absolute, &absolute, &sig, &lit.fields, expected)?
                }
                _ => self.resolve_item_checked(&absolute, &[], true),
            };
            let first_error = match whole_result {
                Ok(ResolvedItem::Type(t)) => return self.literal_target_from_type(node_id, span, t, &[]),
                Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedType(crate::error::TypeResolutionError::NotAType(absolute)),
                    );
                    return None;
                }
                Err(e) => e,
            };
            if absolute.len() >= 3 {
                let (variant, prefix) = absolute.split_last().expect("length checked above");
                let variant_result = match self.resolver.generic_literal_signature(prefix, Some(variant)) {
                    Ok(Some(sig)) => {
                        self.resolve_generic_literal(node_id, span, prefix, prefix, &sig, &lit.fields, expected)?
                    }
                    _ => self.resolve_item_checked(prefix, &[], true),
                };
                if let Ok(ResolvedItem::Type(t)) = variant_result {
                    return self.literal_target_from_type(node_id, span, t, std::slice::from_ref(variant));
                }
            }
            self.error(node_id, span, AnalysisErrorKind::ModuleResolution(first_error));
            return None;
        }

        // Head isn't a module alias -- it must be a type (`Enum::Variant`):
        // local/imported first, then this module's own item, mirroring
        // `resolve_type_qualified_value`'s priority.
        if let Some(head_type) = self.context.find_defined_type(&plain.head).cloned() {
            return self.literal_target_from_type(node_id, span, head_type, &plain.tail);
        }
        if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
            return self.literal_target_from_type(node_id, span, t, &plain.tail);
        }
        let absolute: Vec<Ident> = match alias {
            Some(ImportTarget::GenericItem(absolute)) => absolute,
            _ => self.module_path.iter().cloned().chain(std::iter::once(plain.head.clone())).collect(),
        };
        let variant = (plain.tail.len() == 1).then(|| &plain.tail[0]);
        let result = match self.generic_literal_signature_with_ambient(std::slice::from_ref(&plain.head), &absolute, variant) {
            Some((real_absolute, sig)) => self.resolve_generic_literal(
                node_id,
                span,
                std::slice::from_ref(&plain.head),
                &real_absolute,
                &sig,
                &lit.fields,
                expected,
            )?,
            None => self.resolve_item_checked_with_ambient_fallback(std::slice::from_ref(&plain.head), &absolute, &[]),
        };
        let kind = match result {
            Ok(ResolvedItem::Type(t)) => {
                return self.literal_target_from_type(node_id, span, t, &plain.tail);
            }
            Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => AnalysisErrorKind::NotAModule { name: plain.head.clone() },
            Err(ResolveError::UnknownItem { .. }) => AnalysisErrorKind::UndefinedPathHead {
                name: plain.head.clone(),
                similar_module: self.similar_import_alias(&plain.head),
                similar_type: self.context.similar_type_name(&plain.head).or_else(|| {
                    self.resolver.similar_item_name(&self.module_path, &plain.head, ItemNamespace::Type)
                }),
            },
            Err(e) => AnalysisErrorKind::ModuleResolution(e),
        };
        self.error(node_id, span, kind);
        None
    }

    /// `resolver.generic_literal_signature(absolute, variant)`, retried
    /// against the `core` ambient fallback (see `ModuleResolver::
    /// ambient_core_candidates`) when `prefix` is a genuinely unqualified
    /// single segment and the direct lookup finds nothing generic there --
    /// so a bare, unimported `Option::Some { ... }`'s own generic-ness is
    /// discovered before inference runs, not just its final type. Hands
    /// back whichever absolute path actually matched, since
    /// `expected_matches_generic_item` needs the real declaration's path.
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
        let ambient = self.resolver.ambient_core_candidates(&self.module_path, single).ok().flatten()?;
        let sig = self.resolver.generic_literal_signature(&ambient, variant).ok().flatten()?;
        Some((ambient, sig))
    }

    /// Once `absolute` is confirmed generic, infers its omitted type
    /// arguments (see `infer_literal_type_args`) and resolves it with them
    /// -- the shared tail of every "plain path, no explicit generics"
    /// branch in `resolve_literal_target`. `None` means inference itself
    /// already reported a diagnostic; the caller must give up (`?`).
    fn resolve_generic_literal(
        &mut self,
        node_id: HirId,
        span: Span,
        prefix: &[Ident],
        absolute: &[Ident],
        sig: &GenericLiteralSignature,
        lit_fields: &[HirStructLiteralField],
        expected: Option<&ResolvedType>,
    ) -> Option<Result<ResolvedItem, ResolveError>> {
        let type_args = self.infer_literal_type_args(node_id, span, absolute, sig, lit_fields, expected)?;
        Some(self.resolve_item_checked_with_ambient_fallback(prefix, absolute, &type_args))
    }

    /// Infers the concrete type arguments for a generic literal-
    /// construction target (or a bare unit-variant reference, which passes
    /// an empty `lit_fields`): an `expected` type naming the exact same
    /// declaration wins outright when available (covers the zero-field
    /// case, e.g. `Option::None` assigned into an `Option<i32>`-typed
    /// binding); otherwise, duck-typed unification against `lit_fields`'
    /// own bottom-up-analyzed values, mirroring
    /// `Analyzer::finish_generic_call`. `None` means a dedicated diagnostic
    /// was already reported.
    pub(super) fn infer_literal_type_args(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: &[Ident],
        sig: &GenericLiteralSignature,
        lit_fields: &[HirStructLiteralField],
        expected: Option<&ResolvedType>,
    ) -> Option<Vec<ResolvedType>> {
        if let Some(type_args) = Self::expected_matches_generic_item(expected, absolute) {
            return Some(type_args);
        }
        let subst = self.probe_literal_type_args(sig, lit_fields)?;
        match resolve_inferred_type_args(&sig.generics, &sig.defaults, &subst) {
            Ok(type_args) => Some(type_args),
            Err(_) => {
                let missing: Vec<Ident> = sig
                    .generics
                    .iter()
                    .zip(&sig.defaults)
                    .filter(|(g, default)| default.is_none() && !subst.contains_key(*g))
                    .map(|(g, _)| g.clone())
                    .collect();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UnresolvedLiteralGeneric {
                        r#type: absolute.last().cloned().expect("an absolute path always has a last segment"),
                        generics: missing,
                    },
                );
                None
            }
        }
    }

    /// `expected`'s own type arguments, when it resolves to the exact same
    /// struct/union/enum declaration `absolute` names -- compared by
    /// declaration identity, not by any already-bound type arguments (there
    /// are none yet, that's what this is deducing).
    fn expected_matches_generic_item(expected: Option<&ResolvedType>, absolute: &[Ident]) -> Option<Vec<ResolvedType>> {
        let expected = expected?;
        let (name, module) = absolute.split_last()?;
        let (cell_module, cell_name, type_args) = match expected {
            ResolvedType::Struct(cell) => {
                let c = cell.borrow();
                (c.module_path.clone(), c.name.clone(), c.type_args.clone())
            }
            ResolvedType::Union(cell) => {
                let c = cell.borrow();
                (c.module_path.clone(), c.name.clone(), c.type_args.clone())
            }
            ResolvedType::Enum { cell, .. } => {
                let c = cell.borrow();
                (c.module_path.clone(), c.name.clone(), c.type_args.clone())
            }
            _ => return None,
        };
        (cell_module == module && &cell_name == name).then_some(type_args)
    }

    /// Duck-typed unification of `sig`'s raw declared field types against
    /// `lit_fields`' own values -- unmatched/unknown field names simply
    /// contribute nothing (the real `check_field_initializers` pass reports
    /// those precisely). Fields are analyzed in written order, each against
    /// whatever `expected_for_generic_param` can derive from the ones
    /// already checked -- the same eager precedence
    /// `Analyzer::infer_generic_args` gives ordinary call arguments.
    /// Diagnostics from a *successful* probe are discarded (same
    /// truncate-on-success pattern as `classify_for_in_source` in
    /// `stmts.rs`) since the real pass re-derives them; a field whose value
    /// fails to analyze for an unrelated reason keeps its diagnostics and
    /// this returns `None`, so that real error surfaces directly instead of
    /// a confusing "cannot infer" message.
    fn probe_literal_type_args(
        &mut self,
        sig: &GenericLiteralSignature,
        lit_fields: &[HirStructLiteralField],
    ) -> Option<HashMap<Ident, ResolvedType>> {
        let errors_before = self.errors.len();
        let warnings_before = self.warnings.len();
        let mut subst = HashMap::new();
        let mut ok = true;
        for field in lit_fields {
            let Some((_, raw_type)) = sig.fields.iter().find(|(name, _)| name == &field.name) else { continue };
            let expected =
                self.expected_for_generic_param(field.value.id, field.value.span, raw_type, &sig.generics, &sig.defaults, &subst);
            match self.analyze_expr(&field.value, expected.as_ref()) {
                Some(checked) => unify_generic_type(&sig.generics, raw_type, &checked.r#type, &mut subst),
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

    /// Interprets an already-resolved type (plus at most one trailing path
    /// segment) as a literal's target.
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
                        Some(index) => return Some(LiteralTarget::EnumVariant(cell.clone(), index)),
                        None => {
                            let e = cell.borrow();
                            AnalysisErrorKind::NoSuchEnumMember {
                                r#enum: e.name.clone(),
                                name: variant_name.clone(),
                                similar_variant: best_match(variant_name, e.variants.iter().map(|v| &v.name)),
                                similar_function: best_match(variant_name, e.functions.iter().map(|(name, _)| name)),
                            }
                        }
                    }
                }
                _ => AnalysisErrorKind::GenericPathTooDeep { r#type: cell.borrow().name.clone() },
            },
            _ if rest.is_empty() => AnalysisErrorKind::StructLiteralNotAStruct { found: r#type.clone() },
            _ => AnalysisErrorKind::StaticAccessOnNonStruct { found: r#type.clone() },
        };
        self.error(node_id, span, kind);
        None
    }

    /// Resolves a number literal's target type (see
    /// `default_or_expected_number_type`) and parses/range-checks its text
    /// against that type (see `parse_number_literal`). `NumberExpr` keeps
    /// its digits as plain text precisely so this is the only place that
    /// ever has to interpret them.
    pub(super) fn analyze_number(
        &mut self,
        node_id: HirId,
        span: Span,
        n: &NumberExpr,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let invalid_suffix = |this: &mut Self, ident: &Ident| {
            this.error(node_id, span, AnalysisErrorKind::InvalidNumberType(ident.clone()));
        };

        let resolved_type = match &n.explicit_type {
            Some(explicit_type) => match self.context.resolve_type(
                Type::Named(explicit_type.clone().into()),
                &mut *self.resolver,
                &self.module_path,
                true,
                !self.reveal_stack.is_empty(),
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

        // A literal written with a decimal point must resolve to a float
        // type; a based (hex/octal/binary) literal never carries one, so a
        // float suffix on one (e.g. `0xFFf32`) is rejected here too.
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
                unreachable!("the default type is only Float when a fraction was written, which implies Decimal");
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
                AnalysisErrorKind::NumberLiteralOutOfRange { literal: literal_text, r#type: resolved_type },
            );
            return None;
        };

        Some(CheckedExprNode { id: node_id, span, r#type: resolved_type, kind: CheckedExpr::Number(value) })
    }

    /// Picks the concrete type an *unsuffixed* literal resolves to: `expected`
    /// if given and its numeric family agrees with the literal's own (`Float`
    /// iff a fractional part was written, never the other way), else the
    /// plain `i32`/`f32` default -- see `docs/01-primitives.md` for why the
    /// float default is `f32`, not `f64`. An explicit suffix always wins
    /// and never reaches this (see `analyze_number`).
    fn default_or_expected_number_type(n: &NumberExpr, expected: Option<&ResolvedType>, pointer_bits: u32) -> ResolvedType {
        let default = if n.fractional_part.is_some() { ResolvedType::F32 } else { ResolvedType::I32 };
        let Some(expected) = expected else { return default };
        let Some(kind) = expected.numeric_kind(pointer_bits) else { return default };
        if matches!(kind, NumericKind::Float(_)) == n.fractional_part.is_some() {
            expected.clone()
        } else {
            default
        }
    }

    /// Whether `expr` is a bare (or singly-negated) *unsuffixed* number
    /// literal -- the one expression shape whose concrete type isn't
    /// already pinned, so it's worth peeking at before fully analyzing it:
    /// overload resolution's viability scoring needs to know "is this
    /// argument still open to adapt" without the side effects a real
    /// `analyze_expr` call would commit to. `Negate` is peeked through
    /// because it's transparent to a literal's own type (`-100` is exactly
    /// as adaptable as `100`).
    pub(super) fn adaptable_literal(expr: &HirExprNode) -> bool {
        match &expr.expr {
            HirExpr::Number(n) => n.explicit_type.is_none(),
            HirExpr::Negate(inner) => matches!(&inner.expr, HirExpr::Number(n) if n.explicit_type.is_none()),
            _ => false,
        }
    }

    /// `[a, b, c]` -- a fixed-size array value.
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
        // Widened for the same reason an `if`'s branches are -- an
        // array of mixed variants of one enum is an array of that
        // enum.
        let item_type = declared_item_type.cloned().unwrap_or_else(|| checked_first.r#type.widened());

        let mut checked_elements = Vec::with_capacity(elements.len());
        let check_element = |this: &mut Self, id: HirId, elem_span: Span, checked: CheckedExprNode| {
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
            checked_elements.push(check_element(self, element.id, element.span, checked_element)?);
        }

        let size = checked_elements.len() as u32;
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::SizedArray(Box::new(item_type.clone()), size),
            kind: CheckedExpr::ArrayLiteral(CheckedArrayLiteral { item_type, elements: checked_elements }),
        })
    }
}
