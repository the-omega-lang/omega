use super::*;

struct EnumHeader {
    tag_type: ResolvedType,
    has_tag: bool,
    fields: Vec<ResolvedField>,
}

impl EnumHeader {
    fn claims(&self, name: &Ident) -> bool {
        name.as_ref() == "tag" || self.fields.iter().any(|field| &field.name == name)
    }
}

impl<'r> Analyzer<'r> {
    pub fn signature_of_gap(
        &mut self,
        gap: &omega_hir::HirGapDef,
    ) -> Option<crate::resolved_type::ResolvedGap> {
        let mut functions = Vec::with_capacity(gap.functions.len());
        let mut seen = HashMap::new();
        let mut ok = true;
        for function in &gap.functions {
            if let Some(previous) = seen.insert(function.name.clone(), function.span) {
                self.error(
                    function.id,
                    function.name_span,
                    AnalysisErrorKind::Redeclaration {
                        name: function.name.clone(),
                        previous: Some(previous),
                    },
                );
                ok = false;
                continue;
            }
            let mut params = Vec::with_capacity(function.params.len());
            for param in &function.params {
                let Some(r#type) =
                    self.resolve_type_or_error(param.id, param.span, &param.r#type, true)
                else {
                    ok = false;
                    continue;
                };
                params.push(ResolvedFunctionParam::described(
                    param.ident.clone(),
                    r#type,
                ));
            }
            let Some(return_type) = self.resolve_return_type_or_error(
                function.id,
                function.span,
                &function.return_type,
                true,
            ) else {
                ok = false;
                continue;
            };
            functions.push((
                function.name.clone(),
                crate::resolved_type::GapFunction {
                    decl_id: function.id,
                    span: function.span,
                    visibility: function.visibility,
                    fn_type: ResolvedFunctionType {
                        params,
                        return_type: Box::new(return_type),
                        is_variadic: false,
                        self_mode: None,
                        calling_convention: crate::resolved_type::CallingConvention::Omega,
                    },
                },
            ));
        }
        ok.then_some(crate::resolved_type::ResolvedGap {
            id: gap.id,
            name: gap.name.clone(),
            module_path: self.module_path.clone(),
            span: gap.span,
            functions,
        })
    }
    pub fn analyze_declaration(
        &mut self,
        decl: &HirDeclaration,
        storage: Storage,
        policy: DeclarationPolicy,
    ) -> Option<CheckedDeclaration> {
        // A global's type is never itself embedded inline into another
        // type's layout (it isn't a struct field), so it can never be part
        // of an infinite-size cycle -- always indirect.
        let resolved_type = self.resolve_type_or_error(decl.id, decl.span, &decl.r#type, true)?;
        self.declare_binding(
            decl.id,
            decl.span,
            &decl.ident,
            decl.origin,
            resolved_type.clone(),
            storage,
            decl.mutable,
            policy,
        )?;
        Some(CheckedDeclaration {
            id: decl.id,
            span: decl.span,
            ident: decl.ident.clone(),
            r#type: resolved_type,
            mutable: decl.mutable,
            initial_value: None,
        })
    }

    pub fn analyze_comp_declaration(
        &mut self,
        w: &HirWalrusDeclaration,
    ) -> Option<(ResolvedType, ConstValue)> {
        if w.mutable {
            self.error(w.id, w.span, AnalysisErrorKind::MutCompBinding);
            return None;
        }
        let checked = self.analyze_expr(&w.value, None)?;
        let r#type = checked.r#type.clone();
        let value = self.eval_comp(w.id, &checked)?;
        Some((r#type, value))
    }

    pub fn analyze_global_walrus(
        &mut self,
        w: &HirWalrusDeclaration,
    ) -> Option<CheckedDeclaration> {
        let checked = self.analyze_expr(&w.value, None)?;
        self.finish_global_binding(w.id, w.span, &w.ident, w.mutable, &w.value, checked)
    }

    pub fn analyze_global_declaration_with_init(
        &mut self,
        decl: &HirDeclaration,
        value: &HirExprNode,
    ) -> Option<CheckedDeclaration> {
        let (_, checked_value) =
            self.resolve_typed_decl_init(decl.id, decl.span, &decl.r#type, value)?;
        self.finish_global_binding(
            decl.id,
            decl.span,
            &decl.ident,
            decl.mutable,
            value,
            checked_value,
        )
    }

    pub(super) fn resolve_typed_decl_init(
        &mut self,
        decl_id: HirId,
        decl_span: Span,
        r#type: &Type,
        value: &HirExprNode,
    ) -> Option<(ResolvedType, CheckedExprNode)> {
        if let Type::InferredArray(item) = r#type {
            let item_type = self.resolve_type_or_error(decl_id, decl_span, item, true)?;
            let expected = ResolvedType::SizedArray(Box::new(item_type.clone()), 0);
            let checked_value = self.analyze_expr(value, Some(&expected))?;
            let checked_value = self.coerce_to_expected(Some(&expected), checked_value);
            let ResolvedType::SizedArray(_, size) = &checked_value.r#type else {
                self.error(
                    value.id,
                    value.span,
                    AnalysisErrorKind::ArraySizeNotInferable,
                );
                return None;
            };
            let resolved_type = ResolvedType::SizedArray(Box::new(item_type), *size);
            return Some((resolved_type, checked_value));
        }

        let resolved_type = self.resolve_type_or_error(decl_id, decl_span, r#type, true)?;
        let checked_value = self.analyze_expr(value, Some(&resolved_type))?;
        let checked_value = self.coerce_to_expected(Some(&resolved_type), checked_value);
        if !resolved_type.accepts(&checked_value.r#type) {
            self.error(
                value.id,
                value.span,
                AnalysisErrorKind::AssignmentTypeMismatch {
                    target: resolved_type,
                    value: checked_value.r#type,
                },
            );
            return None;
        }
        Some((resolved_type, checked_value))
    }

    fn finish_global_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        mutable: bool,
        raw_value: &HirExprNode,
        checked_value: CheckedExprNode,
    ) -> Option<CheckedDeclaration> {
        let r#type = checked_value.r#type.clone();
        let const_value = match checked_value.kind {
            CheckedExpr::Const(v) => v,
            _ => self.recognize_top_level_literal(raw_value, &r#type)?,
        };
        self.declare_binding(
            id,
            span,
            ident,
            Origin::default(),
            r#type.clone(),
            Storage::Global,
            mutable,
            DeclarationPolicy::Unique,
        )?;
        Some(CheckedDeclaration {
            id,
            span,
            ident: ident.clone(),
            r#type,
            mutable,
            initial_value: Some(const_value),
        })
    }

    fn recognize_top_level_literal(
        &mut self,
        expr: &HirExprNode,
        expected: &ResolvedType,
    ) -> Option<ConstValue> {
        let not_comp = |this: &mut Self| {
            this.error(expr.id, expr.span, AnalysisErrorKind::TopLevelValueNotComp);
            None
        };
        match &expr.expr {
            HirExpr::Number(n) => self
                .const_number(expr.id, expr.span, n, expected, false)
                .map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => self
                    .const_number(expr.id, expr.span, n, expected, true)
                    .map(ConstValue::Number),
                _ => not_comp(self),
            },
            HirExpr::String(s) => match expected {
                ResolvedType::Str { mutable: false } => Some(ConstValue::Str(s.0.clone())),
                _ => not_comp(self),
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => not_comp(self),
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => not_comp(self),
            },
            HirExpr::ArrayLiteral(elements) => match expected {
                ResolvedType::SizedArray(item, size) if elements.len() == *size as usize => {
                    let mut values = Vec::with_capacity(elements.len());
                    for element in elements {
                        values.push(self.recognize_top_level_literal(element, item)?);
                    }
                    Some(ConstValue::Array(values))
                }
                _ => not_comp(self),
            },
            HirExpr::AddressOf(HirAddressOf {
                base,
                mutable: false,
            }) => match &base.expr {
                HirExpr::ArrayLiteral(elements) => match expected {
                    ResolvedType::Slice {
                        item,
                        mutable: false,
                    } => {
                        let mut values = Vec::with_capacity(elements.len());
                        for element in elements {
                            values.push(self.recognize_top_level_literal(element, item)?);
                        }
                        Some(ConstValue::Slice(values))
                    }
                    _ => not_comp(self),
                },
                _ => not_comp(self),
            },
            _ => not_comp(self),
        }
    }

    pub fn analyze_foreign_binding(
        &mut self,
        binding: &omega_hir::HirForeignBinding,
    ) -> Option<(ResolvedType, crate::annotations::ResolvedAnnotations)> {
        self.check_redundant_hidden(binding.id, binding.explicit_hidden_span);
        let resolved_type =
            self.resolve_type_or_error(binding.id, binding.span, &binding.r#type, true)?;
        // A non-function binding is an external data symbol, not a call
        // boundary, so no calling convention applies to it.
        if let ResolvedType::Function(fn_type) = &resolved_type {
            let fn_type = fn_type.clone();
            if !self.check_signature_abi(binding.id, binding.span, &fn_type) {
                return None;
            }
        }
        let annotations = crate::annotations::resolve(
            self,
            binding.id,
            &binding.annotations,
            crate::annotations::ItemKind::ForeignBinding,
            false,
            false,
            crate::annotations::ManglingMode::Disabled,
        );
        let storage = if matches!(resolved_type, ResolvedType::Function(_)) {
            Storage::Function
        } else {
            Storage::Global
        };
        self.declare_binding(
            binding.id,
            binding.span,
            &binding.ident,
            Origin::default(),
            resolved_type.clone(),
            storage,
            false,
            DeclarationPolicy::Unique,
        )?;
        Some((resolved_type, annotations))
    }

    pub fn collect_foreign_function_signature(
        &mut self,
        f: &omega_hir::HirForeignFunction,
    ) -> Option<(
        ResolvedFunctionType,
        crate::annotations::ResolvedAnnotations,
    )> {
        self.check_redundant_hidden(f.id, f.explicit_hidden_span);
        if !f.generics.is_empty() {
            self.error(
                f.id,
                f.span,
                AnalysisErrorKind::GenericForeignFunctionUnsupported,
            );
            return None;
        }
        let params = self.analyze_all(&f.params, |this, p| {
            this.resolve_type_or_error(p.id, p.span, &p.r#type, true)
                .map(|t| ResolvedFunctionParam::described(p.ident.clone(), t))
        })?;
        let return_type = self.resolve_return_type_or_error(f.id, f.span, &f.return_type, true)?;
        let calling_convention = match self
            .context
            .resolve_convention(f.convention.as_ref().map(|c| &c.name))
        {
            Ok(cc) => cc,
            Err(e) => {
                self.error(f.id, f.span, AnalysisErrorKind::UnresolvedType(e));
                return None;
            }
        };
        if f.is_variadic && !calling_convention.supports_variadic() {
            self.error(
                f.id,
                f.span,
                AnalysisErrorKind::UnresolvedType(
                    TypeResolutionError::VariadicNotSupportedByConvention {
                        convention: calling_convention,
                    },
                ),
            );
            return None;
        }
        let fn_type = ResolvedFunctionType {
            params,
            return_type: Box::new(return_type),
            is_variadic: f.is_variadic,
            self_mode: None,
            calling_convention,
        };
        if !self.check_signature_abi(f.id, f.span, &fn_type) {
            return None;
        }
        let annotations = crate::annotations::resolve(
            self,
            f.id,
            &f.annotations,
            crate::annotations::ItemKind::ForeignFunction,
            false,
            !f.generics.is_empty(),
            crate::annotations::ManglingMode::Disabled,
        );
        Some((fn_type, annotations))
    }

    pub fn check_foreign_function_body(
        &mut self,
        f: &omega_hir::HirForeignFunction,
        fn_type: &ResolvedFunctionType,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedForeignFunctionDef> {
        let Some(body) = &f.body else {
            return Some(CheckedForeignFunctionDef {
                id: f.id,
                span: f.span,
                name: f.name.clone(),
                calling_convention: fn_type.calling_convention,
                is_variadic: fn_type.is_variadic,
                params: fn_type
                    .params
                    .iter()
                    .zip(&f.params)
                    .map(|(param, p)| CheckedParam {
                        id: p.id,
                        span: p.span,
                        ident: p.ident.clone(),
                        r#type: param.r#type.clone(),
                    })
                    .collect(),
                return_type: (*fn_type.return_type).clone(),
                body: None,
                mangling: annotations.mangling.clone(),
            });
        };
        let ((params, checked_body), scope) = self.with_scope(|this| {
            let params = this.analyze_all(&f.params, Self::analyze_param);
            this.current_return_type = (*fn_type.return_type).clone();
            let checked_body = this.analyze_block(body, Some(fn_type.return_type.as_ref()));
            (params, checked_body)
        });
        self.warn_unused_bindings(scope, true);
        let params = params?;
        let checked_body = checked_body?;
        self.check_function_return(
            f.id,
            f.return_type_span,
            &fn_type.return_type,
            &checked_body,
        )?;
        Some(CheckedForeignFunctionDef {
            id: f.id,
            span: f.span,
            name: f.name.clone(),
            calling_convention: fn_type.calling_convention,
            is_variadic: fn_type.is_variadic,
            params,
            return_type: (*fn_type.return_type).clone(),
            body: Some(checked_body),
            mangling: annotations.mangling.clone(),
        })
    }

    fn analyze_param(&mut self, param: &HirParam) -> Option<CheckedParam> {
        // A parameter is passed by value at the call site, not laid out
        // inline -- taking `Self` by value must not be flagged as a layout
        // cycle.
        let resolved_type =
            self.resolve_type_or_error(param.id, param.span, &param.r#type, true)?;
        self.declare_binding(
            param.id,
            param.span,
            &param.ident,
            param.origin,
            resolved_type.clone(),
            Storage::Parameter,
            false,
            DeclarationPolicy::Unique,
        )?;
        Some(CheckedParam {
            id: param.id,
            span: param.span,
            ident: param.ident.clone(),
            r#type: resolved_type,
        })
    }

    fn analyze_struct_fields(&mut self, fields: &[HirField]) -> Option<Vec<CheckedField>> {
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        self.analyze_all(fields, |this, field| {
            if let Some(previous) = seen.insert(field.ident.clone(), field.name_span) {
                this.error(
                    field.id,
                    field.name_span,
                    AnalysisErrorKind::Redeclaration {
                        name: field.ident.clone(),
                        previous: Some(previous),
                    },
                );
                return None;
            }
            // A field genuinely lays its type out inline -- the case
            // `RecursiveTypeWithoutIndirection` exists to catch.
            let resolved_type =
                this.resolve_type_or_error(field.id, field.span, &field.r#type, false)?;
            Some(CheckedField {
                id: field.id,
                span: field.span,
                ident: field.ident.clone(),
                r#type: resolved_type,
            })
        })
    }

    fn check_function_return(
        &mut self,
        id: HirId,
        span: Span,
        return_type: &ResolvedType,
        body: &CheckedBlock,
    ) -> Option<()> {
        match Self::block_type(body) {
            None => Some(()),
            Some(found) if return_type.accepts(&found) => Some(()),
            Some(found) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::ReturnTypeMismatch {
                        expected: return_type.clone(),
                        found,
                    },
                );
                None
            }
        }
    }

    pub fn collect_function_signature(
        &mut self,
        f: &HirFunctionDef,
    ) -> Option<(
        ResolvedFunctionType,
        crate::annotations::ResolvedAnnotations,
    )> {
        let f = &self.normalized_function(f)?;
        self.check_redundant_hidden(f.id, f.explicit_hidden_span);
        let params = self.analyze_all(&f.params, |this, p| {
            this.resolve_type_or_error(p.id, p.span, &p.r#type, true)
                .map(|t| ResolvedFunctionParam::described(p.ident.clone(), t))
        })?;

        for (p, r#type) in f
            .params
            .iter()
            .zip(params.iter().map(|param| &param.r#type))
        {
            if matches!(
                r#type,
                ResolvedType::Struct(_)
                    | ResolvedType::Union(_)
                    | ResolvedType::Enum { .. }
                    | ResolvedType::AnonymousEnum { .. }
            ) {
                let size =
                    crate::annotations::estimate_type_size(r#type, self.target.pointer_bytes());
                if size > crate::annotations::LARGE_STRUCT_BY_VALUE_THRESHOLD {
                    self.warn(
                        p.id,
                        p.span,
                        AnalysisWarningKind::LargeStructByValue {
                            r#type: r#type.clone(),
                            size,
                        },
                    );
                }
            }
        }

        let return_type = self.resolve_return_type_or_error(f.id, f.span, &f.return_type, true)?;
        let annotations = crate::annotations::resolve(
            self,
            f.id,
            &f.annotations,
            crate::annotations::ItemKind::Function,
            f.self_mode.is_some(),
            !f.generics.is_empty(),
            crate::annotations::ManglingMode::Enabled,
        );
        Some((
            ResolvedFunctionType {
                params,
                return_type: Box::new(return_type),
                is_variadic: false,
                self_mode: f.self_mode,
                calling_convention: crate::resolved_type::CallingConvention::Omega,
            },
            annotations,
        ))
    }

    pub fn check_overload_duplicates(
        &mut self,
        functions: &[HirFunctionDef],
        signatures: &[(
            ResolvedFunctionType,
            crate::annotations::ResolvedAnnotations,
        )],
    ) {
        for i in 1..functions.len() {
            for j in 0..i {
                if functions[i].name != functions[j].name {
                    continue;
                }
                let (sig_i, sig_j) = (&signatures[i].0, &signatures[j].0);
                // The static and member namespaces are independent, so a
                // receiverless and a receiver-bearing declaration never
                // redeclare or overload one another however alike their
                // parameter lists are.
                if FunctionNamespace::of(sig_i) != FunctionNamespace::of(sig_j) {
                    continue;
                }
                let same_params = sig_i.param_types().eq(sig_j.param_types());
                if same_params {
                    self.error(
                        functions[i].id,
                        functions[i].name_span,
                        AnalysisErrorKind::Redeclaration {
                            name: functions[i].name.clone(),
                            previous: Some(functions[j].name_span),
                        },
                    );
                    break;
                }
                // Receiver spelling alone is not an overload selector: an
                // ordinary `value.name(...)` call writes no receiver, so two
                // members that differ only there could never be told apart.
                if FunctionNamespace::of(sig_i) == FunctionNamespace::Member {
                    let same_rest = sig_i.params[1..]
                        .iter()
                        .map(|param| &param.r#type)
                        .eq(sig_j.params[1..].iter().map(|param| &param.r#type));
                    if same_rest {
                        self.error(
                            functions[i].id,
                            functions[i].name_span,
                            AnalysisErrorKind::AmbiguousSelfOverload {
                                name: functions[i].name.clone(),
                                previous: functions[j].name_span,
                            },
                        );
                        break;
                    }
                }
            }
        }
    }

    fn item_annotations(
        &mut self,
        id: HirId,
        annotations: &[omega_hir::HirAnnotation],
        kind: crate::annotations::ItemKind,
    ) -> crate::annotations::ResolvedAnnotations {
        crate::annotations::resolve(
            self,
            id,
            annotations,
            kind,
            false,
            false,
            crate::annotations::ManglingMode::Enabled,
        )
    }

    fn collect_methods(
        &mut self,
        functions: &[omega_hir::HirFunctionDef],
        method_ids: &[HirId],
    ) -> Option<Vec<(Ident, ResolvedMethod)>> {
        let (signatures, _) = self.with_scope(|this| {
            this.analyze_all(functions, |this, f| this.collect_function_signature(f))
        });
        let signatures = signatures?;
        self.check_overload_duplicates(functions, &signatures);

        let own: Vec<(Ident, ResolvedMethod)> = functions
            .iter()
            .zip(signatures)
            .zip(method_ids)
            .map(|((f, (fn_type, annotations)), &decl_id)| {
                (
                    f.name.clone(),
                    ResolvedMethod {
                        decl_id,
                        fn_type,
                        visibility: f.visibility,
                        annotations,
                        source: None,
                    },
                )
            })
            .collect();

        Some(own)
    }

    pub fn signature_of_struct(
        &mut self,
        s: &HirStructDef,
        cell: &Rc<RefCell<ResolvedStructType>>,
        method_ids: &[HirId],
    ) -> Option<()> {
        self.check_redundant_hidden(s.id, s.explicit_hidden_span);
        let annotations =
            self.item_annotations(s.id, &s.annotations, crate::annotations::ItemKind::Struct);
        cell.borrow_mut().layout = annotations.layout;
        cell.borrow_mut().suppress = annotations.suppress;
        cell.borrow_mut().is_marker = s.is_marker;

        cell.borrow_mut().fields = self.resolve_declared_fields(&s.fields)?;

        let self_type = ResolvedType::Struct(cell.clone());
        if !s.is_marker && crate::layout::is_zero_sized(&self_type) {
            self.error(
                s.id,
                s.span,
                AnalysisErrorKind::ZeroSizedAggregate {
                    name: s.name.clone(),
                    is_union: false,
                    instantiated_at: self.instantiation_site(),
                },
            );
        }

        let functions = self.collect_methods(&s.functions, method_ids)?;
        cell.borrow_mut().functions = functions;
        Some(())
    }

    pub fn signature_of_union(
        &mut self,
        u: &HirUnionDef,
        cell: &Rc<RefCell<ResolvedUnionType>>,
        method_ids: &[HirId],
    ) -> Option<()> {
        self.check_redundant_hidden(u.id, u.explicit_hidden_span);
        let annotations =
            self.item_annotations(u.id, &u.annotations, crate::annotations::ItemKind::Union);
        cell.borrow_mut().suppress = annotations.suppress;

        cell.borrow_mut().fields = self.resolve_declared_fields(&u.fields)?;

        let self_type = ResolvedType::Union(cell.clone());
        if crate::layout::is_zero_sized(&self_type) {
            self.error(
                u.id,
                u.span,
                AnalysisErrorKind::ZeroSizedAggregate {
                    name: u.name.clone(),
                    is_union: true,
                    instantiated_at: self.instantiation_site(),
                },
            );
        }

        let functions = self.collect_methods(&u.functions, method_ids)?;
        cell.borrow_mut().functions = functions;
        Some(())
    }

    fn resolve_declared_fields(&mut self, fields: &[HirField]) -> Option<Vec<ResolvedField>> {
        for field in fields {
            self.check_redundant_hidden(field.id, field.explicit_hidden_span);
        }
        let checked = self.analyze_struct_fields(fields)?;
        Some(
            fields
                .iter()
                .zip(checked)
                .map(|(declared, checked)| {
                    ResolvedField::new(checked.ident, checked.r#type, declared.visibility)
                })
                .collect(),
        )
    }

    fn resolve_enum_header(&mut self, e: &HirEnumDef) -> Option<EnumHeader> {
        let mut ok = true;
        let mut explicit_tag: Option<ResolvedType> = None;
        let mut fields: Vec<ResolvedField> = Vec::new();
        let mut seen: HashMap<Ident, Span> = HashMap::new();

        for (position, field) in e.header.iter().enumerate() {
            self.check_redundant_hidden(field.id, field.explicit_hidden_span);
            if field.ident.as_ref() == "tag" {
                match self.resolve_tag_type(field, position) {
                    Some(tag_type) => explicit_tag = Some(tag_type),
                    None => ok = false,
                }
                continue;
            }
            if seen.insert(field.ident.clone(), field.span).is_some() {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision {
                        field: field.ident.clone(),
                        variant: None,
                    },
                );
                ok = false;
                continue;
            }
            let Some(resolved) =
                self.resolve_type_or_error(field.id, field.span, &field.r#type, false)
            else {
                ok = false;
                continue;
            };
            if !self.const_representable(&resolved) {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumHeaderFieldUnsupportedType {
                        field: field.ident.clone(),
                        found: resolved,
                    },
                );
                ok = false;
                continue;
            }
            fields.push(ResolvedField::new(
                field.ident.clone(),
                resolved,
                field.visibility,
            ));
        }

        ok.then(|| EnumHeader {
            has_tag: explicit_tag.is_some(),
            tag_type: explicit_tag.unwrap_or(ResolvedType::U32),
            fields,
        })
    }

    fn resolve_tag_type(&mut self, field: &HirField, position: usize) -> Option<ResolvedType> {
        if position != 0 {
            self.error(field.id, field.span, AnalysisErrorKind::EnumTagNotFirst);
            return None;
        }
        let tag_type = self.resolve_type_or_error(field.id, field.span, &field.r#type, true)?;
        if !matches!(
            tag_type.numeric_kind(self.target.pointer_bits()),
            Some(NumericKind::Signed(_) | NumericKind::Unsigned(_))
        ) {
            self.error(
                field.id,
                field.span,
                AnalysisErrorKind::EnumTagNotInteger { found: tag_type },
            );
            return None;
        }
        Some(tag_type)
    }

    fn resolve_enum_dynamic_fields(
        &mut self,
        e: &HirEnumDef,
        header: &EnumHeader,
    ) -> Option<Vec<ResolvedField>> {
        let mut ok = true;
        let mut fields: Vec<ResolvedField> = Vec::new();
        let mut seen: HashMap<Ident, Span> = HashMap::new();

        for field in &e.dynamic_fields {
            self.check_redundant_hidden(field.id, field.explicit_hidden_span);
            if header.claims(&field.ident) || seen.contains_key(&field.ident) {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision {
                        field: field.ident.clone(),
                        variant: None,
                    },
                );
                ok = false;
                continue;
            }
            seen.insert(field.ident.clone(), field.span);
            let Some(resolved) =
                self.resolve_type_or_error(field.id, field.span, &field.r#type, false)
            else {
                ok = false;
                continue;
            };
            fields.push(ResolvedField::new(
                field.ident.clone(),
                resolved,
                field.visibility,
            ));
        }

        ok.then_some(fields)
    }

    fn resolve_enum_variants(
        &mut self,
        e: &HirEnumDef,
        header: &EnumHeader,
        dynamic_fields: &[ResolvedField],
    ) -> Option<Vec<ResolvedEnumVariant>> {
        let mut ok = true;
        let mut variants: Vec<ResolvedEnumVariant> = Vec::new();
        let mut seen_variants: HashMap<Ident, Span> = HashMap::new();
        let mut seen_tags: HashMap<i128, (Ident, Span)> = HashMap::new();

        for (declared_index, variant) in e.variants.iter().enumerate() {
            if let Some(previous) = seen_variants.insert(variant.name.clone(), variant.span) {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::Redeclaration {
                        name: variant.name.clone(),
                        previous: Some(previous),
                    },
                );
                ok = false;
                continue;
            }

            let expected_args = header.fields.len() + header.has_tag as usize;
            if variant.args.len() != expected_args {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::EnumVariantArgCount {
                        variant: variant.name.clone(),
                        expected: expected_args,
                        found: variant.args.len(),
                        has_tag: header.has_tag,
                    },
                );
                ok = false;
                continue;
            }

            let Some(tag) = self.resolve_variant_tag(variant, header, declared_index) else {
                ok = false;
                continue;
            };
            let tag_key = match tag {
                NumberValue::Signed(value) => value as i128,
                NumberValue::Unsigned(value) => value as i128,
                NumberValue::Float(_) => unreachable!("tag types are integers"),
            };
            if let Some((previous_variant, previous)) = seen_tags.get(&tag_key) {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::DuplicateEnumTag {
                        variant: variant.name.clone(),
                        value: tag_key.to_string(),
                        previous_variant: previous_variant.clone(),
                        previous: *previous,
                    },
                );
                ok = false;
                continue;
            }
            seen_tags.insert(tag_key, (variant.name.clone(), variant.span));

            let mut header_values = Vec::with_capacity(header.fields.len());
            let mut variant_ok = true;
            for (field, arg) in header
                .fields
                .iter()
                .zip(&variant.args[header.has_tag as usize..])
            {
                match self.const_eval(arg, &field.r#type) {
                    Some(value) => header_values.push(value),
                    None => variant_ok = false,
                }
            }

            let fields =
                self.resolve_variant_fields(variant, header, dynamic_fields, &mut variant_ok);
            if !variant_ok {
                ok = false;
                continue;
            }
            variants.push(ResolvedEnumVariant {
                name: variant.name.clone(),
                tag,
                header_values,
                fields,
            });
        }

        ok.then_some(variants)
    }

    fn resolve_variant_tag(
        &mut self,
        variant: &omega_hir::HirEnumVariant,
        header: &EnumHeader,
        declared_index: usize,
    ) -> Option<NumberValue> {
        if !header.has_tag {
            let (_, max) = header
                .tag_type
                .integer_domain(self.target.pointer_bits())
                .expect("enum tag types are validated before variants are resolved");
            if declared_index as u128 > max as u128 {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::EnumImplicitTagOutOfRange {
                        variant: variant.name.clone(),
                        value: declared_index,
                        r#type: header.tag_type.clone(),
                    },
                );
                return None;
            }
            return Some(NumberValue::Unsigned(declared_index as u64));
        }
        match self.const_eval(&variant.args[0], &header.tag_type)? {
            ConstValue::Number(value) => Some(value),
            _ => unreachable!("const_eval only produces Number for an integer expected type"),
        }
    }

    fn resolve_variant_fields(
        &mut self,
        variant: &omega_hir::HirEnumVariant,
        header: &EnumHeader,
        dynamic_fields: &[ResolvedField],
        ok: &mut bool,
    ) -> Vec<ResolvedField> {
        let mut fields: Vec<ResolvedField> = Vec::new();
        let mut seen: HashMap<Ident, Span> = HashMap::new();

        for field in &variant.fields {
            self.check_redundant_hidden(field.id, field.explicit_hidden_span);
            let shadows_shared = header.claims(&field.ident)
                || dynamic_fields
                    .iter()
                    .any(|shared| shared.name == field.ident);
            if shadows_shared {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision {
                        field: field.ident.clone(),
                        variant: Some(variant.name.clone()),
                    },
                );
                *ok = false;
                continue;
            }
            if let Some(previous) = seen.insert(field.ident.clone(), field.name_span) {
                self.error(
                    field.id,
                    field.name_span,
                    AnalysisErrorKind::Redeclaration {
                        name: field.ident.clone(),
                        previous: Some(previous),
                    },
                );
                *ok = false;
                continue;
            }
            // A body field is inline layout, exactly like a struct field --
            // the one context that catches by-value recursion.
            let Some(resolved) =
                self.resolve_type_or_error(field.id, field.span, &field.r#type, false)
            else {
                *ok = false;
                continue;
            };
            fields.push(ResolvedField::new(
                field.ident.clone(),
                resolved,
                field.visibility,
            ));
        }
        fields
    }

    fn check_variant_name_collisions(&mut self, e: &HirEnumDef) -> bool {
        let mut ok = true;
        let mut variants: HashMap<&Ident, Span> = HashMap::new();
        for variant in &e.variants {
            variants.entry(&variant.name).or_insert(variant.span);
        }
        for function in &e.functions {
            if let Some(previous) = variants.get(&function.name) {
                self.error(
                    function.id,
                    function.name_span,
                    AnalysisErrorKind::Redeclaration {
                        name: function.name.clone(),
                        previous: Some(*previous),
                    },
                );
                ok = false;
            }
        }
        ok
    }

    pub fn signature_of_enum(
        &mut self,
        e: &HirEnumDef,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        method_ids: &[HirId],
    ) -> Option<()> {
        self.check_redundant_hidden(e.id, e.explicit_hidden_span);
        let annotations =
            self.item_annotations(e.id, &e.annotations, crate::annotations::ItemKind::Enum);
        cell.borrow_mut().layout = annotations.layout;
        cell.borrow_mut().suppress = annotations.suppress;

        let header = self.resolve_enum_header(e)?;
        let dynamic_fields = self.resolve_enum_dynamic_fields(e, &header)?;
        let variants = self.resolve_enum_variants(e, &header, &dynamic_fields);
        let names_ok = self.check_variant_name_collisions(e);
        let variants = variants.filter(|_| names_ok)?;

        {
            let mut resolved = cell.borrow_mut();
            resolved.tag_type = header.tag_type;
            resolved.header = header.fields;
            resolved.dynamic_fields = dynamic_fields;
            resolved.variants = variants;
        }

        let functions = self.collect_methods(&e.functions, method_ids)?;
        cell.borrow_mut().functions = functions;
        Some(())
    }
}

mod bodies;

#[cfg(test)]
mod tests;
