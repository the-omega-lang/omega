use super::*;

/// What an owner declares under one name in one associated-function
/// namespace, from the point of view of a call that must infer generic
/// arguments before the declaration has a signature.
pub(super) enum MethodTemplate {
    /// No generic declaration under that name; the ordinary member/static
    /// lookup owns the call and its diagnostics.
    Absent,
    /// The template could not be determined, and the reason was reported.
    Failed,
    Found(GenericMethodTemplate),
}

impl<'r> Analyzer<'r> {
    /// Applies the ordinary argument conversions once a generic call's
    /// concrete signature is known. Inference runs before the parameter
    /// types are concrete, so these arguments were checked without an
    /// expected type; routing them through `coerce_to_expected` is what makes
    /// an already-resolved anonymous-enum parameter behave like any other.
    /// `implicit_params` is how many leading parameters the call supplies
    /// without writing them, so the written arguments line up with the
    /// parameters they actually fill.
    fn coerce_call_arguments(
        &mut self,
        checked_args: Vec<CheckedExprNode>,
        fn_type: &ResolvedFunctionType,
        implicit_params: usize,
    ) -> Option<Vec<CheckedExprNode>> {
        let mut coerced = Vec::with_capacity(checked_args.len());
        let mut ok = true;
        for (index, arg) in checked_args.into_iter().enumerate() {
            let Some(param) = fn_type.params.get(index + implicit_params) else {
                coerced.push(arg);
                continue;
            };
            let expected_type = param.r#type.clone();
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

    /// The generic declaration `owner` makes under `name` in `namespace`.
    ///
    /// Generic declarations are not among an owner's resolved members -- they
    /// have no signature until a call's arguments give them one -- so every
    /// call site looks for the template separately, and
    /// [`MethodTemplate::Absent`] means the ordinary lookup owns the name.
    pub(super) fn generic_method_template(
        &mut self,
        node_id: HirId,
        span: Span,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
    ) -> MethodTemplate {
        match self
            .resolver
            .generic_method_template(owner, name, namespace)
        {
            Ok(Some(template)) => MethodTemplate::Found(template),
            Ok(None) => MethodTemplate::Absent,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                MethodTemplate::Failed
            }
        }
    }

    /// Infers a generic method's arguments from the call and materializes
    /// that instantiation, together with the arguments inference analyzed.
    ///
    /// `implicit_params` is how many of the declaration's leading parameters
    /// the call supplies without writing them -- one for the receiver of
    /// `value.name(...)`, none when the receiver is written out as an
    /// ordinary argument.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn instantiate_generic_method_call(
        &mut self,
        node_id: HirId,
        span: Span,
        owner: &ResolvedType,
        name: &Ident,
        namespace: FunctionNamespace,
        template: &GenericMethodTemplate,
        explicit: &[ResolvedGenericArg],
        implicit_params: usize,
        args: &[HirExprNode],
        expected: Option<&ResolvedType>,
    ) -> Option<(ResolvedMethod, Vec<CheckedExprNode>)> {
        // The owner instantiation binds `Self` and the owner's own
        // parameters, which the declaration's written types still name;
        // explicitly written arguments come first, so they outrank an owner
        // parameter this declaration shadows.
        let mut seed =
            GenericSubstitution::zip(template.generics.iter().map(|param| &param.ident), explicit);
        for (bound, arg) in template.owner_substitution.iter() {
            seed.push(bound.clone(), arg.clone());
        }

        let comp_types = self.comp_param_types(node_id, span, &template.generics, &seed);
        let generics = self.generic_params(&template.generics, &comp_types);
        let params = &template.params[implicit_params.min(template.params.len())..];
        let seed = Self::seed_from_expected(seed, expected, &generics, &template.return_type);
        let (checked_args, subst) = self.infer_generic_args(&generics, params, args, seed)?;

        let generic_args = match resolve_inferred_generic_args(&generics, &subst) {
            Ok(generic_args) => generic_args,
            Err(generic) => {
                if let Some((parameter, found)) =
                    Self::fat_pointer_generic_mismatch(&generics, params, &checked_args, &subst)
                {
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

        match self
            .resolver
            .instantiate_generic_method(owner, name, namespace, &generic_args)
        {
            Ok(Some(method)) => Some((method, checked_args)),
            Ok(None) => None,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
    }

    /// Checks the written argument count against the instantiated signature
    /// and applies the ordinary argument conversions to the arguments
    /// inference already analyzed.
    ///
    /// `written` is counted from the call, not from `checked_args`:
    /// inference stops at the last declared parameter, so a surplus argument
    /// only exists in the written call.
    pub(super) fn finish_generic_call_arguments(
        &mut self,
        node_id: HirId,
        span: Span,
        fn_type: &ResolvedFunctionType,
        implicit_params: usize,
        written: usize,
        checked_args: Vec<CheckedExprNode>,
    ) -> Option<Vec<CheckedExprNode>> {
        let expected = fn_type.params.len() - implicit_params.min(fn_type.params.len());
        if written != expected && !fn_type.is_variadic {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected,
                    found: written,
                },
            );
            return None;
        }
        self.coerce_call_arguments(checked_args, fn_type, implicit_params)
    }

    /// `Owner::name(...)` and `Owner::self::name(receiver, ...)` where the
    /// owner is already concrete and the declaration is generic. Generic
    /// arguments may be written on the function segment; the rest are
    /// inferred from the call like any other generic call.
    pub(crate) fn resolve_generic_method_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(expr_path) = Self::callee_expr_path(call) else {
            return Intercepted::Declined;
        };
        let path = &expr_path.path;
        // A member namespace is selected by the `self` segment, which is
        // part of the owner-to-function path, not of the owner itself.
        let (namespace, member, member_segments) = match path.tail.as_slice() {
            [.., segment, member] if segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT => {
                (FunctionNamespace::Member, member, 2)
            }
            [.., member] => (FunctionNamespace::Static, member, 1),
            [] => return Intercepted::Declined,
        };
        // A path carries at most one written generic argument list: it
        // belongs either to the owner or to the function, and anything else
        // is not a call this resolves. Segment indices count the head as 0,
        // so the owner's last segment is the one the member segments follow.
        let owner_generics_written = !expr_path.generic_args.is_empty()
            && expr_path.args_at + member_segments == path.tail.len();
        let function_generics_written =
            !expr_path.generic_args.is_empty() && expr_path.args_at == path.tail.len();
        if !expr_path.generic_args.is_empty()
            && !owner_generics_written
            && !function_generics_written
        {
            return Intercepted::Declined;
        }

        let Some(owner) = self.callee_owner_type(
            node_id,
            span,
            expr_path,
            member_segments,
            owner_generics_written,
        ) else {
            return Intercepted::Declined;
        };
        let template = match self.generic_method_template(node_id, span, &owner, member, namespace)
        {
            MethodTemplate::Absent => return Intercepted::Declined,
            MethodTemplate::Failed => return Intercepted::Claimed(None),
            MethodTemplate::Found(template) => template,
        };

        let explicit = if function_generics_written {
            let declared = Self::owner_item_path(&owner, member);
            match self.resolve_generic_arg_list(
                node_id,
                span,
                &expr_path.generic_args,
                &declared,
                &template.generics,
            ) {
                Some(explicit) => explicit,
                None => return Intercepted::Claimed(None),
            }
        } else {
            Vec::new()
        };

        Intercepted::Claimed(self.finish_generic_method_call(
            node_id, span, call, expr_path, &owner, member, namespace, &template, &explicit,
            expected,
        ))
    }

    /// The owner a type-qualified call selects from: everything the callee
    /// path writes before its function segments.
    ///
    /// The prefix is resolved as ordinary type syntax, so every spelling that
    /// names a type -- a plain name, an import, a module-qualified path, an
    /// alias, `Self`, an enclosing generic parameter, an explicit generic
    /// application -- selects an owner here exactly as it would in a type
    /// position. `None` means the prefix does not name a type, which is not
    /// this call shape; the ordinary callee path reports whatever is wrong
    /// with it, so nothing is reported here.
    fn callee_owner_type(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
        member_segments: usize,
        owner_generics_written: bool,
    ) -> Option<ResolvedType> {
        let mut owner_path = expr_path.path.clone();
        owner_path
            .tail
            .truncate(expr_path.path.tail.len() - member_segments);
        let written = if owner_generics_written {
            Type::Generic(owner_path, expr_path.generic_args.clone())
        } else {
            Type::Named(owner_path)
        };
        self.without_diagnostics(|this| this.resolve_type_or_error(node_id, span, &written, true))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_generic_method_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expr_path: &ExprPath,
        owner: &ResolvedType,
        member: &Ident,
        namespace: FunctionNamespace,
        template: &GenericMethodTemplate,
        explicit: &[ResolvedGenericArg],
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // Reached through its owner rather than an instance, a member's
        // receiver is an ordinary written argument, so nothing is implicit.
        let (method, checked_args) = self.instantiate_generic_method_call(
            node_id, span, owner, member, namespace, template, explicit, 0, &call.args, expected,
        )?;

        let (owner_module_path, owner_id) = owner
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(
            method.visibility,
            &owner_module_path,
            owner_id,
            expr_path.path.origin,
        ) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: owner.clone(),
                },
            );
            return None;
        }

        let fn_type = method.value_fn_type();
        let checked_args = self.finish_generic_call_arguments(
            node_id,
            span,
            &fn_type,
            0,
            call.args.len(),
            checked_args,
        )?;
        Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            method.decl_id,
            Storage::Function,
            fn_type,
            checked_args,
        ))
    }

    /// How a diagnostic names the declaration a generic call reached: the
    /// owner's own path with the function appended.
    pub(super) fn owner_item_path(owner: &ResolvedType, member: &Ident) -> Vec<Ident> {
        let owner = owner.autoderef();
        let mut path = owner
            .declaring_owner()
            .map(|(module_path, _)| module_path)
            .unwrap_or_default();
        path.push(Self::owner_name(owner));
        path.push(member.clone());
        path
    }

    /// `GenericOwner::name(...)` and `GenericOwner::self::name(receiver, ...)`
    /// where the owner's type arguments are inferred from the call. A member
    /// call infers them from its explicit receiver argument like any other.
    pub(crate) fn resolve_generic_owner_function_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };
        let (namespace, member) = match path.tail.as_slice() {
            [member] => (FunctionNamespace::Static, member),
            [segment, member] if segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT => {
                (FunctionNamespace::Member, member)
            }
            _ => return Intercepted::Declined,
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

        let Some((real_absolute, sig)) = self.generic_owner_function_signature_with_ambient(
            &accessor,
            std::slice::from_ref(&path.head),
            &absolute,
            member,
            namespace,
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
        Intercepted::Claimed(self.finish_generic_owner_function_call(
            node_id,
            span,
            call,
            &accessor,
            std::slice::from_ref(&path.head),
            &owner,
            member,
            namespace,
            &sig,
            expected,
            path.origin,
        ))
    }

    fn generic_owner_function_signature_with_ambient(
        &mut self,
        accessor: &[Ident],
        prefix: &[Ident],
        absolute: &[Ident],
        function_name: &Ident,
        namespace: FunctionNamespace,
    ) -> Option<(Vec<Ident>, GenericOwnerFunctionSignature)> {
        if let Ok(Some(sig)) =
            self.resolver
                .generic_owner_function_signature(absolute, function_name, namespace)
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
            .generic_owner_function_signature(&ambient, function_name, namespace)
            .ok()
            .flatten()?;
        Some((ambient, sig))
    }

    /// The substitution call inference starts from. Explicitly written
    /// arguments are already bound here and outrank the expected result: the
    /// caller asked for them, so a conflicting expectation must be reported
    /// by the ordinary result check rather than reinterpreted.
    fn seed_from_expected(
        explicit: GenericSubstitution,
        expected: Option<&ResolvedType>,
        generics: &GenericParams<'_>,
        return_type: &Type,
    ) -> GenericSubstitution {
        let mut seed = explicit;
        if let Some(expected) = expected {
            let mut inferred = GenericSubstitution::new();
            unify_generic_type(generics, return_type, expected, &mut inferred);
            for (generic, resolved) in inferred.iter() {
                seed.bind_if_absent(generic, || resolved.widened());
            }
        }
        seed
    }

    fn fat_pointer_generic_mismatch(
        generics: &GenericParams<'_>,
        params: &[Type],
        args: &[CheckedExprNode],
        subst: &GenericSubstitution,
    ) -> Option<(Ident, ResolvedType)> {
        for (raw, arg) in params.iter().zip(args) {
            let Type::Pointer(inner, _) = raw else {
                continue;
            };
            let Type::Named(path) = inner.as_ref() else {
                continue;
            };
            if !path.is_unqualified()
                || !generics.names().any(|name| name == &path.head)
                || subst.contains(&path.head)
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

    #[allow(clippy::too_many_arguments)]
    fn finish_generic_owner_function_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &[Ident],
        prefix: &[Ident],
        owner: &ItemAccess,
        member: &Ident,
        namespace: FunctionNamespace,
        sig: &GenericOwnerFunctionSignature,
        expected: Option<&ResolvedType>,
        origin: Origin,
    ) -> Option<CheckedExprNode> {
        let comp_types = self.comp_param_types(
            node_id,
            span,
            &sig.owner_generics,
            &GenericSubstitution::new(),
        );
        let generics = self.generic_params(&sig.owner_generics, &comp_types);
        let (checked_args, subst) = self.infer_generic_args(
            &generics,
            &sig.params,
            &call.args,
            Self::seed_from_expected(
                GenericSubstitution::new(),
                expected,
                &generics,
                &sig.return_type,
            ),
        )?;

        let generic_args = match resolve_inferred_generic_args(&generics, &subst) {
            Ok(generic_args) => generic_args,
            Err(_) => {
                let missing: Vec<Ident> = sig
                    .owner_generics
                    .iter()
                    .filter(|param| param.default.is_none() && !subst.contains(&param.ident))
                    .map(|param| param.ident.clone())
                    .collect();
                if let Some((parameter, found)) = Self::fat_pointer_generic_mismatch(
                    &generics,
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
            match self.resolve_item_with_ambient_from(accessor, prefix, owner, &generic_args) {
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

        let method = owner_type
            .candidates_in(namespace, member)
            .expect("generic owner signatures only come from aggregate types")
            .into_iter()
            .next()
            .expect("generic_owner_function_signature confirmed this function exists");

        let (owner_module_path, owner_id) = owner_type
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(method.visibility, &owner_module_path, owner_id, origin) {
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

        let decl_id = method.decl_id;
        // A member reached this way is an ordinary function value: the
        // receiver is the first written argument, never adapted.
        let fn_type = method.value_fn_type();
        let checked_args = self.finish_generic_call_arguments(
            node_id,
            span,
            &fn_type,
            0,
            call.args.len(),
            checked_args,
        )?;

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
        let Some(expr_path) = Self::callee_expr_path(call) else {
            return Intercepted::Declined;
        };
        let path = &expr_path.path;
        // Generic arguments written on an earlier segment name a generic
        // owner, not this function; that stays with generic-place resolution.
        if !expr_path.generic_args.is_empty() && expr_path.args_at != path.tail.len() {
            return Intercepted::Declined;
        }

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

        let explicit = match self.resolve_generic_arg_list(
            node_id,
            span,
            &expr_path.generic_args,
            &access.absolute,
            &sig.generics,
        ) {
            Some(explicit) => explicit,
            None => return Intercepted::Claimed(None),
        };

        Intercepted::Claimed(self.finish_generic_call(
            node_id, span, call, &accessor, &access, &sig, &explicit, expected,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &[Ident],
        access: &ItemAccess,
        sig: &GenericSignature,
        explicit: &[ResolvedGenericArg],
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // Written arguments bind the declaration's generics left to right;
        // inference only ever fills what is left.
        let bound = GenericSubstitution::zip(sig.generics.iter().map(|p| &p.ident), explicit);

        let comp_types = self.comp_param_types(node_id, span, &sig.generics, &bound);
        let generics = self.generic_params(&sig.generics, &comp_types);
        let (checked_args, subst) = self.infer_generic_args(
            &generics,
            &sig.params,
            &call.args,
            Self::seed_from_expected(bound, expected, &generics, &sig.return_type),
        )?;

        let generic_args = match resolve_inferred_generic_args(&generics, &subst) {
            Ok(generic_args) => generic_args,
            Err(generic) => {
                if let Some((parameter, found)) = Self::fat_pointer_generic_mismatch(
                    &generics,
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
            &generic_args,
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

        let checked_args = self.finish_generic_call_arguments(
            node_id,
            span,
            &fn_type,
            0,
            call.args.len(),
            checked_args,
        )?;

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
