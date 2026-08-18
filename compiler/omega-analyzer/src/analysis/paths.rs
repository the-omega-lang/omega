use super::*;

impl<'r> Analyzer<'r> {
    pub fn resolve_gap_path(
        &mut self,
        id: HirId,
        span: Span,
        path: &Path,
    ) -> Option<std::rc::Rc<crate::resolved_type::ResolvedGap>> {
        let absolute = match self.context.resolve_absolute_item_path(
            &mut *self.resolver,
            path,
            &self.module_path,
        ) {
            Ok(absolute) => absolute,
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::UnresolvedType(error));
                return None;
            }
        };
        match self.resolve_item_checked(&absolute, &[], true) {
            Ok(ResolvedItem::Gap(gap)) => Some(gap),
            Ok(_) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::GlueTargetNotGap {
                        target: absolute
                            .last()
                            .cloned()
                            .expect("an absolute path has a name"),
                    },
                );
                None
            }
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
    }
    pub(super) fn resolve_alias(
        &mut self,
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        self.resolver.resolve_import_alias(&self.module_path, alias)
    }

    pub(super) fn resolve_alias_or_error(
        &mut self,
        node_id: HirId,
        span: Span,
        alias: &Ident,
    ) -> Option<Option<ImportTarget>> {
        match self.resolve_alias(alias) {
            Ok(target) => Some(target),
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                None
            }
        }
    }

    pub(super) fn resolve_path_alias_or_error(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &Path,
    ) -> Option<Option<ImportTarget>> {
        let module = self.path_module(path);
        match self.resolver.resolve_import_alias(&module, &path.head) {
            Ok(target) => Some(target),
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
    }

    pub(super) fn similar_import_alias(&mut self, target: &Ident) -> Option<Ident> {
        best_match(
            target,
            self.resolver.import_alias_names(&self.module_path).iter(),
        )
    }

    pub(super) fn resolve_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        written_path: &Path,
        accessor: &[Ident],
        absolute: Vec<Ident>,
        unqualified: Option<&Ident>,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        if !self.check_macro_dependency_visibility(node_id, span, written_path, &absolute) {
            return None;
        }
        if let Some((name, module_path)) = absolute.split_last()
            && let Ok(Some(candidates)) = self
                .resolver
                .function_overload_signatures(module_path, name)
        {
            let signatures: Vec<(HirId, ResolvedFunctionType)> = candidates
                .iter()
                .map(|(id, fn_type, _)| (*id, fn_type.clone()))
                .collect();
            if let Some(ResolvedType::Function(expected_fn)) = expected
                && let Some((decl_id, fn_type)) =
                    Self::unique_overload_signature_match(expected_fn, &signatures)
            {
                let visibility = candidates
                    .iter()
                    .find(|(id, ..)| *id == decl_id)
                    .map(|(_, _, v)| *v)
                    .expect("decl_id came from this same candidates list");
                if !self.check_visibility(visibility, module_path) {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                            module: module_path.to_vec(),
                            item: name.clone(),
                        }),
                    );
                    return None;
                }
                let r#type = ResolvedType::Function(fn_type);
                let root = CheckedPlaceRoot::Variable {
                    decl_id,
                    storage: Storage::Function,
                    r#type: r#type.clone(),
                };
                return Some((root, r#type, false));
            }
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: candidates.into_iter().map(|(_, t, _)| t).collect(),
                },
            );
            return None;
        }
        match self.resolver.resolve_item(accessor, &absolute, &[], true, false) {
            Ok(ResolvedItem::Value {
                r#type,
                storage,
                decl_id,
                mutable,
            }) => {
                let root = CheckedPlaceRoot::Variable {
                    decl_id,
                    storage,
                    r#type: r#type.clone(),
                };
                Some((root, r#type, mutable))
            }
            Ok(ResolvedItem::Type(_)) => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Ok(ResolvedItem::Gap(_)) => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Err(ResolveError::UnknownItem { .. }) if unqualified.is_some() => {
                let name = unqualified.expect("checked by the guard").clone();
                let similar = self.context.similar_variable_name(&name).or_else(|| {
                    self.resolver
                        .similar_item_name(accessor, &name, ItemNamespace::Value)
                });
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UndefinedVariable { name, similar },
                );
                None
            }
            Err(ResolveError::UnknownModule(missing))
                if missing.len() + 1 == absolute.len() && missing == absolute[..missing.len()] =>
            {
                match self.resolve_item_checked(&missing, &[], true) {
                    Ok(ResolvedItem::Type(t)) => self
                        .resolve_type_member(node_id, span, &t, &absolute[missing.len()..])
                        .map(|(root, r#type)| (root, r#type, false)),
                    Ok(ResolvedItem::Gap(gap)) => self
                        .resolve_gap_member(node_id, span, &gap, &absolute[missing.len()..])
                        .map(|(root, r#type)| (root, r#type, false)),
                    _ => {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(
                                missing,
                            )),
                        );
                        None
                    }
                }
            }
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                None
            }
        }
    }

    pub(super) fn unique_overload_signature_match(
        expected: &ResolvedFunctionType,
        candidates: &[(HirId, ResolvedFunctionType)],
    ) -> Option<(HirId, ResolvedFunctionType)> {
        let mut matches = candidates.iter().filter(|(_, fn_type)| {
            fn_type.is_variadic == expected.is_variadic
                && fn_type.self_mode == expected.self_mode
                && fn_type.return_type == expected.return_type
                && fn_type.params.len() == expected.params.len()
                && fn_type
                    .params
                    .iter()
                    .zip(&expected.params)
                    .all(|((_, a), (_, b))| a == b)
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.clone())
    }

    pub(super) fn resolve_type_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &omega_parser::prelude::Path,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        if path.head.as_ref() == "str" {
            return self.resolve_type_member(
                node_id,
                span,
                &ResolvedType::Str { mutable: false },
                &path.tail,
            );
        }
        if let Some(head_type) = self.context.find_defined_type(&path.head).cloned() {
            return self.resolve_type_member(node_id, span, &head_type, &path.tail);
        }

        let alias = self.resolve_path_alias_or_error(node_id, span, path)?;
        if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
            return self.resolve_type_member(node_id, span, &t, &path.tail);
        }
        if let Some(ImportTarget::Item(_, ResolvedItem::Gap(gap))) = alias {
            return self.resolve_gap_member(node_id, span, &gap, &path.tail);
        }
        let absolute: Vec<Ident> = match alias {
            Some(ImportTarget::GenericItem(absolute)) | Some(ImportTarget::Module(absolute)) => {
                absolute
            }
            _ => self
                .path_module(path)
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect(),
        };
        let variant = path.tail.first();
        let result = match self.generic_literal_signature_with_ambient(
            std::slice::from_ref(&path.head),
            &absolute,
            variant,
        ) {
            Some((real_absolute, sig)) => {
                let type_args = self.infer_literal_type_args(
                    node_id,
                    span,
                    &real_absolute,
                    &sig,
                    &[],
                    expected,
                )?;
                self.resolve_item_checked_with_ambient_fallback(
                    std::slice::from_ref(&path.head),
                    &real_absolute,
                    &type_args,
                )
            }
            None => self.resolve_item_checked_with_ambient_fallback(
                std::slice::from_ref(&path.head),
                &absolute,
                &[],
            ),
        };
        let kind = match result {
            Ok(ResolvedItem::Type(t)) => {
                return self.resolve_type_member(node_id, span, &t, &path.tail);
            }
            Ok(ResolvedItem::Gap(gap)) => {
                return self.resolve_gap_member(node_id, span, &gap, &path.tail);
            }
            Ok(ResolvedItem::Value { .. }) => AnalysisErrorKind::NotAModule {
                name: path.head.clone(),
            },
            Err(ResolveError::UnknownItem { .. }) => AnalysisErrorKind::UndefinedPathHead {
                name: path.head.clone(),
                similar_module: self.similar_import_alias(&path.head),
                similar_type: self.context.similar_type_name(&path.head).or_else(|| {
                    self.resolver.similar_item_name(
                        &self.module_path,
                        &path.head,
                        ItemNamespace::Type,
                    )
                }),
            },
            Err(e) => AnalysisErrorKind::ModuleResolution(e),
        };
        self.error(node_id, span, kind);
        None
    }

    pub(super) fn resolve_generic_args_place(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let segments = expr_path.path.segments();
        let rest = &segments[expr_path.args_at + 1..];
        if rest.len() > 1 {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::GenericPathTooDeep {
                    r#type: segments[expr_path.args_at].clone(),
                },
            );
            return None;
        }

        let type_args = self.resolve_generic_arg_list(node_id, span, expr_path)?;
        let prefix = &segments[..=expr_path.args_at];
        let absolute = self.generic_prefix_absolute(node_id, span, &expr_path.path, prefix)?;
        let accessor = self.path_module(&expr_path.path);
        match self.resolve_item_with_ambient_from(&accessor, prefix, &absolute, &type_args) {
            Ok(ResolvedItem::Type(_)) if rest.is_empty() => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Ok(ResolvedItem::Type(t)) => self.resolve_type_member(node_id, span, &t, rest),
            Ok(ResolvedItem::Value {
                r#type,
                storage,
                decl_id,
                mutable: _,
            }) if rest.is_empty() => {
                let root = CheckedPlaceRoot::Variable {
                    decl_id,
                    storage,
                    r#type: r#type.clone(),
                };
                Some((root, r#type))
            }
            Ok(ResolvedItem::Value { .. }) => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NotAModule {
                        name: segments[expr_path.args_at].clone(),
                    },
                );
                None
            }
            Ok(ResolvedItem::Gap(_)) => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                None
            }
        }
    }

    pub(super) fn resolve_generic_arg_list(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
    ) -> Option<Vec<ResolvedType>> {
        self.analyze_all(&expr_path.generic_args, |this, arg| {
            this.resolve_type_or_error(node_id, span, arg, true)
        })
    }

    pub(super) fn generic_prefix_absolute(
        &mut self,
        node_id: HirId,
        span: Span,
        written_path: &Path,
        prefix: &[Ident],
    ) -> Option<Vec<Ident>> {
        let module = self.path_module(written_path);
        if let [single] = prefix {
            if let Some(ImportTarget::GenericItem(absolute)) =
                match self.resolver.resolve_import_alias(&module, single) {
                    Ok(alias) => Some(alias),
                    Err(error) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                        None
                    }
                }?
            {
                return Some(absolute);
            }
            return Some(
                module
                    .iter()
                    .cloned()
                    .chain(std::iter::once(single.clone()))
                    .collect(),
            );
        }
        let head = &prefix[0];
        match self.resolver.resolve_import_alias(&module, head) {
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
            Ok(Some(ImportTarget::Module(target))) => Some(
                target
                    .into_iter()
                    .chain(prefix[1..].iter().cloned())
                    .collect(),
            ),
            Ok(_) => {
                let similar_module = best_match(head, self.resolver.import_alias_names(&module).iter());
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UndefinedPathHead {
                        name: head.clone(),
                        similar_module,
                        similar_type: self.context.similar_type_name(head),
                    },
                );
                None
            }
        }
    }

    fn resolve_gap_member(
        &mut self,
        node_id: HirId,
        span: Span,
        gap: &std::rc::Rc<crate::resolved_type::ResolvedGap>,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        if rest.len() != 1 {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NotAModule {
                    name: gap.name.clone(),
                },
            );
            return None;
        }
        let member = &rest[0];
        let Some((_, function)) = gap.functions.iter().find(|(name, _)| name == member) else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: gap.name.clone(),
                    function: member.clone(),
                    similar: best_match(member, gap.functions.iter().map(|(name, _)| name)),
                },
            );
            return None;
        };
        let r#type = ResolvedType::Function(function.fn_type.clone());
        Some((
            CheckedPlaceRoot::Variable {
                decl_id: function.decl_id,
                storage: Storage::Function,
                r#type: r#type.clone(),
            },
            r#type,
        ))
    }

    fn resolve_type_member(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: &ResolvedType,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let member = &rest[0];
        let (type_name, mut method, missing_member_error, mut owner_module_path, mut owner_id) =
            match r#type {
                ResolvedType::Struct(cell) => {
                    let struct_type = cell.borrow();
                    let method = struct_type
                        .functions
                        .iter()
                        .find(|(name, _)| name == member)
                        .map(|(_, method)| method.clone());
                    let similar = match method {
                        Some(_) => None,
                        None => {
                            best_match(member, struct_type.functions.iter().map(|(name, _)| name))
                        }
                    };
                    let missing = AnalysisErrorKind::NoSuchStructFunction {
                        r#struct: struct_type.name.clone(),
                        function: member.clone(),
                        similar,
                    };
                    (
                        struct_type.name.clone(),
                        method,
                        missing,
                        struct_type.module_path.clone(),
                        struct_type.id,
                    )
                }
                ResolvedType::Union(cell) => {
                    let union_type = cell.borrow();
                    let method = union_type
                        .functions
                        .iter()
                        .find(|(name, _)| name == member)
                        .map(|(_, method)| method.clone());
                    let similar = match method {
                        Some(_) => None,
                        None => {
                            best_match(member, union_type.functions.iter().map(|(name, _)| name))
                        }
                    };
                    let missing = AnalysisErrorKind::NoSuchStructFunction {
                        r#struct: union_type.name.clone(),
                        function: member.clone(),
                        similar,
                    };
                    (
                        union_type.name.clone(),
                        method,
                        missing,
                        union_type.module_path.clone(),
                        union_type.id,
                    )
                }
                ResolvedType::Enum { cell, .. } => {
                    let found = cell.borrow().variant(member).map(|(i, v)| (i, v.clone()));
                    if let Some((variant_index, variant)) = found {
                        return self.resolve_unit_variant(
                            node_id,
                            span,
                            cell,
                            variant_index,
                            &variant,
                            rest,
                        );
                    }
                    let e = cell.borrow();
                    let method = e
                        .functions
                        .iter()
                        .find(|(name, _)| name == member)
                        .map(|(_, method)| method.clone());
                    let missing = AnalysisErrorKind::NoSuchEnumMember {
                        r#enum: e.name.clone(),
                        name: member.clone(),
                        similar_variant: best_match(member, e.variants.iter().map(|v| &v.name)),
                        similar_function: best_match(
                            member,
                            e.functions.iter().map(|(name, _)| name),
                        ),
                    };
                    (e.name.clone(), method, missing, e.module_path.clone(), e.id)
                }
                other => {
                    let methods = match self.resolver.primitive_methods(other) {
                        Ok(methods) => methods,
                        Err(err) => {
                            self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                            return None;
                        }
                    };
                    if methods.is_empty() {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::StaticAccessOnNonStruct {
                                found: other.clone(),
                            },
                        );
                        return None;
                    }
                    let type_name = Ident(other.to_string());
                    let method = methods
                        .iter()
                        .find(|(name, _)| name == member)
                        .map(|(_, m)| m.clone());
                    let similar = match method {
                        Some(_) => None,
                        None => best_match(member, methods.iter().map(|(name, _)| name)),
                    };
                    let missing = AnalysisErrorKind::NoSuchStructFunction {
                        r#struct: type_name.clone(),
                        function: member.clone(),
                        similar,
                    };
                    (type_name, method, missing, Vec::new(), node_id)
                }
            };

        if method.is_none() {
            let conformances = match self.resolver.conformances_for_type(r#type) {
                Ok(conformances) => conformances,
                Err(err) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                    return None;
                }
            };
            let candidates: Vec<_> = conformances
                .iter()
                .flat_map(|conform| {
                    conform
                        .methods
                        .iter()
                        .filter(|(name, method)| {
                            name == member && method.fn_type.self_mode.is_none()
                        })
                        .map(move |(_, method)| (conform, method.clone()))
                })
                .collect();
            if candidates.len() > 1 {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::AmbiguousConformanceStatic {
                        target: r#type.to_string(),
                        function: member.clone(),
                        specs: candidates
                            .iter()
                            .map(|(conform, _)| conform.spec.borrow().name.clone())
                            .collect(),
                    },
                );
                return None;
            }
            if let Some((conform, conformance_method)) = candidates.into_iter().next() {
                let spec = conform.spec.borrow();
                owner_module_path = spec.module_path.clone();
                owner_id = spec.id;
                method = Some(conformance_method);
            }
        }

        let Some(method) = method else {
            self.error(node_id, span, missing_member_error);
            return None;
        };
        if rest.len() > 1 {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::StructPathTooDeep {
                    r#struct: type_name,
                    function: member.clone(),
                },
            );
            return None;
        }
        if !self.check_member_visibility(method.visibility, &owner_module_path, owner_id) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: r#type.clone(),
                },
            );
            return None;
        }
        if method.fn_type.self_mode.is_some() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MemberFunctionWithoutInstance {
                    r#struct: type_name,
                    function: member.clone(),
                },
            );
            return None;
        }

        let fn_type = ResolvedType::Function(method.fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id: method.decl_id,
            storage: Storage::Function,
            r#type: fn_type.clone(),
        };
        Some((root, fn_type))
    }

    fn resolve_unit_variant(
        &mut self,
        node_id: HirId,
        span: Span,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        variant_index: usize,
        variant: &ResolvedEnumVariant,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        if rest.len() > 1 {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::GenericPathTooDeep {
                    r#type: variant.name.clone(),
                },
            );
            return None;
        }
        let dynamic_field_names: Vec<Ident> = cell
            .borrow()
            .dynamic_fields
            .iter()
            .map(|(n, _, _)| n.clone())
            .collect();
        if !dynamic_field_names.is_empty() || !variant.fields.is_empty() {
            let fields = dynamic_field_names
                .into_iter()
                .chain(variant.fields.iter().map(|(name, _, _)| name.clone()))
                .collect();
            self.error(
                node_id,
                span,
                AnalysisErrorKind::EnumVariantMissingBody {
                    r#enum: cell.borrow().name.clone(),
                    variant: variant.name.clone(),
                    fields,
                },
            );
            return None;
        }
        let r#type = ResolvedType::Enum {
            cell: cell.clone(),
            variant: Some(variant_index),
        };
        let construct = CheckedExprNode {
            id: node_id,
            span,
            r#type: r#type.clone(),
            kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct {
                variant_index,
                fields: vec![],
            }),
        };
        Some((CheckedPlaceRoot::Expr(Box::new(construct)), r#type))
    }
}
