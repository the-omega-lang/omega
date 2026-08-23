use super::*;

impl<'r> Analyzer<'r> {
    /// Applies the ordinary argument conversions once a generic call's
    /// concrete signature is known. Inference runs before the parameter
    /// types are concrete, so these arguments were checked without an
    /// expected type; routing them through `coerce_to_expected` is what makes
    /// an already-resolved anonymous-enum parameter behave like any other.
    fn coerce_call_arguments(
        &mut self,
        checked_args: Vec<CheckedExprNode>,
        fn_type: &ResolvedFunctionType,
    ) -> Option<Vec<CheckedExprNode>> {
        let mut coerced = Vec::with_capacity(checked_args.len());
        let mut ok = true;
        for (index, arg) in checked_args.into_iter().enumerate() {
            let Some((_, expected_type)) = fn_type.params.get(index) else {
                coerced.push(arg);
                continue;
            };
            let expected_type = expected_type.clone();
            let arg = self.coerce_to_expected(Some(&expected_type), arg);
            if !expected_type.accepts(&arg.r#type) {
                self.error(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type,
                        found: arg.r#type.clone(),
                    },
                );
                ok = false;
                continue;
            }
            coerced.push(arg);
        }
        ok.then_some(coerced)
    }

    pub(crate) fn resolve_generic_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };
        let [member] = path.tail.as_slice() else {
            return Intercepted::Declined;
        };
        if self.context.find_defined_type(&path.head).is_some() {
            return Intercepted::Declined;
        }

        let accessor = self.path_module(path);
        // The owner is `path.head` alone; an explicit anchor names it
        // directly, so it is never looked up as an import binding.
        let access =
            match self.anchored_prefix(node_id, span, path, std::slice::from_ref(&path.head)) {
                AnchoredPath::Failed => return Intercepted::Claimed(None),
                AnchoredPath::Absolute(absolute) => ItemAccess::gated(absolute),
                AnchoredPath::Unanchored => {
                    let alias = self
                        .resolver
                        .resolve_import_alias(&accessor, &path.head)
                        .ok()
                        .flatten();
                    match alias {
                        Some(ImportTarget::Item(absolute, _)) => ItemAccess::gated(absolute),
                        Some(ImportTarget::ItemPath(access)) => access,
                        Some(ImportTarget::Module(_)) => return Intercepted::Declined,
                        None => ItemAccess::gated(
                            accessor
                                .iter()
                                .cloned()
                                .chain(std::iter::once(path.head.clone()))
                                .collect(),
                        ),
                    }
                }
            };
        let absolute = access.absolute.clone();

        let Some((real_absolute, sig)) = self.generic_static_function_signature_with_ambient(
            &accessor,
            std::slice::from_ref(&path.head),
            &absolute,
            member,
        ) else {
            return Intercepted::Declined;
        };
        if !sig.function_generics.is_empty() {
            return Intercepted::Declined;
        }

        // The authorization the binding carried belongs to the path it named:
        // the ambient-`core` fallback below found a different owner, which
        // this reference was never authorized for.
        let owner = ItemAccess {
            bypass_visibility: access.bypass_visibility && real_absolute == absolute,
            absolute: real_absolute,
        };
        Intercepted::Claimed(self.finish_generic_static_call(
            node_id,
            span,
            call,
            &accessor,
            std::slice::from_ref(&path.head),
            &owner,
            member,
            &sig,
            expected,
        ))
    }

    fn generic_static_function_signature_with_ambient(
        &mut self,
        accessor: &[Ident],
        prefix: &[Ident],
        absolute: &[Ident],
        function_name: &Ident,
    ) -> Option<(Vec<Ident>, GenericStaticFunctionSignature)> {
        if let Ok(Some(sig)) = self
            .resolver
            .generic_static_function_signature(absolute, function_name)
        {
            return Some((absolute.to_vec(), sig));
        }
        let [single] = prefix else { return None };
        let ambient = self
            .resolver
            .ambient_core_candidates(accessor, single)
            .ok()
            .flatten()?;
        let sig = self
            .resolver
            .generic_static_function_signature(&ambient, function_name)
            .ok()
            .flatten()?;
        Some((ambient, sig))
    }

    fn seed_from_expected(
        expected: Option<&ResolvedType>,
        generics: &[Ident],
        return_type: &Type,
    ) -> HashMap<Ident, ResolvedType> {
        let mut seed = HashMap::new();
        if let Some(expected) = expected {
            unify_generic_type(generics, return_type, expected, &mut seed);
            for resolved in seed.values_mut() {
                *resolved = resolved.widened();
            }
        }
        seed
    }

    fn fat_pointer_generic_mismatch(
        generics: &[Ident],
        params: &[Type],
        args: &[CheckedExprNode],
        subst: &HashMap<Ident, ResolvedType>,
    ) -> Option<(Ident, ResolvedType)> {
        for (raw, arg) in params.iter().zip(args) {
            let Type::Pointer(inner, _) = raw else {
                continue;
            };
            let Type::Named(path) = inner.as_ref() else {
                continue;
            };
            if !path.is_unqualified()
                || !generics.contains(&path.head)
                || subst.contains_key(&path.head)
            {
                continue;
            }
            if matches!(
                arg.r#type,
                ResolvedType::Slice { .. } | ResolvedType::Str { .. }
            ) {
                return Some((path.head.clone(), arg.r#type.clone()));
            }
        }
        None
    }

    fn finish_generic_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &[Ident],
        prefix: &[Ident],
        owner: &ItemAccess,
        member: &Ident,
        sig: &GenericStaticFunctionSignature,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (checked_args, subst) = self.infer_generic_args(
            &sig.owner_generics,
            &sig.owner_defaults,
            &sig.params,
            &call.args,
            Self::seed_from_expected(expected, &sig.owner_generics, &sig.return_type),
        )?;

        let type_args =
            match resolve_inferred_type_args(&sig.owner_generics, &sig.owner_defaults, &subst) {
                Ok(type_args) => type_args,
                Err(_) => {
                    let missing: Vec<Ident> = sig
                        .owner_generics
                        .iter()
                        .zip(&sig.owner_defaults)
                        .filter(|(g, default)| default.is_none() && !subst.contains_key(*g))
                        .map(|(g, _)| g.clone())
                        .collect();
                    if let Some((parameter, found)) = Self::fat_pointer_generic_mismatch(
                        &sig.owner_generics,
                        &sig.params,
                        &checked_args,
                        &subst,
                    ) {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::GenericParamFromFatPointer { parameter, found },
                        );
                    } else {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::UnresolvedLiteralGeneric {
                                r#type: owner
                                    .absolute
                                    .last()
                                    .cloned()
                                    .expect("an absolute path always has a last segment"),
                                generics: missing,
                            },
                        );
                    }
                    return None;
                }
            };

        let owner_type =
            match self.resolve_item_with_ambient_from(accessor, prefix, owner, &type_args) {
                Ok(ResolvedItem::Type(t)) => t,
                Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                    self.error(node_id, span, AnalysisErrorKind::UnresolvedCallee);
                    return None;
                }
                Err(e) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                    return None;
                }
            };

        let all_methods = owner_type
            .declared_methods()
            .expect("generic static signatures only come from aggregate types");
        let method = all_methods
            .into_iter()
            .find(|(name, m)| name == member && m.fn_type.self_mode.is_none())
            .map(|(_, m)| m)
            .expect("generic_static_function_signature confirmed this static function exists");

        let (owner_module_path, owner_id) = owner_type
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(method.visibility, &owner_module_path, owner_id) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: owner_type.clone(),
                },
            );
            return None;
        }

        let ResolvedMethod {
            decl_id, fn_type, ..
        } = method;
        if checked_args.len() != fn_type.params.len() && !fn_type.is_variadic {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: fn_type.params.len(),
                    found: checked_args.len(),
                },
            );
            return None;
        }
        let checked_args = self.coerce_call_arguments(checked_args, &fn_type)?;

        Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            Storage::Function,
            fn_type,
            checked_args,
        ))
    }

    pub(crate) fn resolve_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };

        if path.is_unqualified()
            && self
                .context
                .find_variable(&path.head, path.origin)
                .is_some()
        {
            return Intercepted::Declined;
        }

        let accessor = self.path_module(path);
        let access: ItemAccess = if path.is_unqualified() {
            match self
                .resolver
                .resolve_import_alias(&accessor, &path.head)
                .ok()
                .flatten()
            {
                Some(ImportTarget::ItemPath(access)) => access,
                _ => ItemAccess::gated(
                    accessor
                        .iter()
                        .cloned()
                        .chain(std::iter::once(path.head.clone()))
                        .collect(),
                ),
            }
        } else {
            match self.module_qualified_path(node_id, span, path) {
                ModuleQualifiedPath::Item(access) => access,
                ModuleQualifiedPath::NotModule => return Intercepted::Declined,
                ModuleQualifiedPath::Failed => return Intercepted::Claimed(None),
            }
        };

        let sig: GenericSignature = match self.resolver.generic_function_signature(&access.absolute)
        {
            Ok(Some(sig)) => sig,
            Ok(None) => return Intercepted::Declined,
            Err(_) => return Intercepted::Declined,
        };

        Intercepted::Claimed(
            self.finish_generic_call(node_id, span, call, &accessor, &access, &sig, expected),
        )
    }

    fn finish_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &[Ident],
        access: &ItemAccess,
        sig: &GenericSignature,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        let (checked_args, subst) = self.infer_generic_args(
            &sig.generics,
            &sig.defaults,
            &sig.params,
            &call.args,
            Self::seed_from_expected(expected, &sig.generics, &sig.return_type),
        )?;

        let type_args = match resolve_inferred_type_args(&sig.generics, &sig.defaults, &subst) {
            Ok(type_args) => type_args,
            Err(generic) => {
                if let Some((parameter, found)) = Self::fat_pointer_generic_mismatch(
                    &sig.generics,
                    &sig.params,
                    &checked_args,
                    &subst,
                ) {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::GenericParamFromFatPointer { parameter, found },
                    );
                } else {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedGenericParam(generic),
                    );
                }
                return None;
            }
        };

        let (fn_type, storage, decl_id) = match self.resolver.resolve_item(
            accessor,
            &access.absolute,
            &type_args,
            access.options(ResolveItemOptions::INDIRECT),
        ) {
            Ok(ResolvedItem::Value {
                r#type: ResolvedType::Function(fn_type),
                storage,
                decl_id,
                mutable: _,
            }) => (fn_type, storage, decl_id),
            Ok(_) => {
                self.error(node_id, span, AnalysisErrorKind::UnresolvedCallee);
                return None;
            }
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                return None;
            }
        };

        if checked_args.len() != fn_type.params.len() && !fn_type.is_variadic {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: fn_type.params.len(),
                    found: checked_args.len(),
                },
            );
            return None;
        }
        let checked_args = self.coerce_call_arguments(checked_args, &fn_type)?;

        Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            storage,
            fn_type,
            checked_args,
        ))
    }
}
