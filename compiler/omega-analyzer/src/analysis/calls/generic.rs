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
            if !Self::value_type_compatible(&expected_type, &arg.r#type) {
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
        explicit: &WrittenGenerics,
        implicit_params: usize,
        args: &[HirExprNode],
        expected: Expected<'_>,
    ) -> Option<(ResolvedMethod, Vec<CheckedExprNode>)> {
        // The owner instantiation already fixed `Self` and the owner's own
        // parameters in the template, so only the declaration's own
        // positions are open; explicitly written arguments bind first.
        let signature = &template.template;
        let mut inference =
            Inference::of_template(signature, &template.enclosing, explicit.bindings.clone());
        inference.seed_from_expected(&signature.return_type, expected);
        let params = &signature.params[implicit_params.min(signature.params.len())..];
        let (checked_args, generic_args) =
            self.solve_generic_call(node_id, span, name, &mut inference, params, args, explicit)?;
        let target = crate::resolver::GenericCallTarget::Method {
            owner,
            name,
            namespace,
        };
        let generic_args = self.complete_call_generic_args(node_id, span, target, generic_args)?;
        self.check_written_selectors(node_id, span, name, target, explicit, &generic_args)?;

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

    /// Infers a call's generic arguments from its arguments, reporting the
    /// first position nothing determined. The arguments are analyzed once,
    /// whatever the outcome.
    #[allow(clippy::too_many_arguments)]
    fn solve_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        inference: &mut Inference<'_>,
        params: &[TypePattern],
        args: &[HirExprNode],
        explicit: &WrittenGenerics,
    ) -> Option<(Vec<CheckedExprNode>, Vec<Option<ResolvedGenericArg>>)> {
        let checked_args = self.infer_arguments(inference, params, args)?;
        match Self::solved_generic_args(inference) {
            Ok(generic_args) => Some((checked_args, generic_args)),
            Err(position) => {
                if let Some((parameter, found)) =
                    inference.fat_pointer_mismatch(params, &checked_args)
                {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::GenericParamFromFatPointer { parameter, found },
                    );
                } else {
                    self.undetermined_generic_param(
                        node_id,
                        span,
                        name,
                        inference.generics,
                        explicit,
                        position,
                    );
                }
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
        expected: Expected<'_>,
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
        let Some(lists) = Self::member_generics(expr_path, member_segments) else {
            return Intercepted::Declined;
        };
        let Some(owner) =
            self.callee_owner_type(node_id, span, expr_path, member_segments, lists.owner)
        else {
            return Intercepted::Declined;
        };
        let template = match self.generic_method_template(node_id, span, &owner, member, namespace)
        {
            MethodTemplate::Absent => return Intercepted::Declined,
            MethodTemplate::Failed => return Intercepted::Claimed(None),
            MethodTemplate::Found(template) => template,
        };

        let explicit = if let Some(function_generics) = lists.function {
            let declared = Self::owner_item_path(&owner, member);
            match self.resolve_written_generics(
                node_id,
                span,
                function_generics,
                &declared,
                &template.template.generics,
            ) {
                Some(explicit) => explicit,
                None => return Intercepted::Claimed(None),
            }
        } else {
            WrittenGenerics::default()
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
    pub(crate) fn callee_owner_type(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
        member_segments: usize,
        owner_generics: Option<&[ExprGenericArg]>,
    ) -> Option<ResolvedType> {
        // A selector is not owner syntax, so this is not the owner-qualified
        // reading; the ordinary path reading reports it.
        let owner_generics = match owner_generics {
            Some(written) => Some(Self::plain_generic_args(written)?),
            None => None,
        };
        let written = Self::owner_type_syntax(expr_path, member_segments, owner_generics);
        self.without_diagnostics(|this| this.resolve_type_or_error(node_id, span, &written, true))
    }

    fn owner_type_syntax(
        expr_path: &ExprPath,
        member_segments: usize,
        owner_generics: Option<Vec<GenericArg>>,
    ) -> Type {
        let mut owner_path = expr_path.path.clone();
        owner_path
            .tail
            .truncate(expr_path.path.tail.len() - member_segments);
        match owner_generics {
            Some(owner_generics) => Type::Generic(owner_path, owner_generics),
            None => Type::Named(owner_path),
        }
    }

    /// The owner of `Owner<A>::name<B>`, reported when it does not resolve.
    /// Both lists are written, so the owner list must be complete: a `_` in
    /// it would need joint owner-and-member inference, which is not yet
    /// supported (`docs/issues/language-limitations.md`).
    pub(crate) fn owner_type_with_generics(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
        member_segments: usize,
        lists: &MemberGenerics<'_>,
    ) -> Option<ResolvedType> {
        let written = lists.owner.unwrap_or_default();
        let owner_generics = self.ordinary_generic_args(node_id, span, written, "a type")?;
        if let Some(hole) = owner_generics
            .iter()
            .position(|arg| *arg == GenericArg::Infer)
        {
            let path = &expr_path.path;
            let segments = path.segments();
            let prefix = &segments[..=path.tail.len() - member_segments];
            let access = self.generic_prefix_absolute(node_id, span, path, prefix)?;
            let params = self.item_generic_params_for(&self.path_module(path), prefix, &access);
            if let Some(param) = params.get(hole) {
                let name = prefix
                    .last()
                    .expect("a prefix has its owner segment")
                    .clone();
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UndeterminedInferenceHole {
                        name,
                        parameter: param.label(),
                    },
                );
                return None;
            }
        }
        let written = Self::owner_type_syntax(expr_path, member_segments, Some(owner_generics));
        self.resolve_type_or_error(node_id, span, &written, true)
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
        explicit: &WrittenGenerics,
        expected: Expected<'_>,
    ) -> Option<CheckedExprNode> {
        // Reached through its owner rather than an instance, a member's
        // receiver is an ordinary written argument, so nothing is implicit.
        let (method, checked_args) = self.instantiate_generic_method_call(
            node_id, span, owner, member, namespace, template, explicit, 0, &call.args, expected,
        )?;

        if !self.check_visibility(
            method.visibility,
            &method.declaring_module,
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

    /// A generic call's inferred arguments with every gap filled from the
    /// declaration's own defaults.
    fn complete_call_generic_args(
        &mut self,
        node_id: HirId,
        span: Span,
        target: crate::resolver::GenericCallTarget<'_>,
        inferred: Vec<Option<ResolvedGenericArg>>,
    ) -> Option<Vec<ResolvedGenericArg>> {
        if inferred.iter().all(Option::is_some) {
            return Some(inferred.into_iter().flatten().collect());
        }
        match self.resolver.prepare_generic_call(target, &inferred) {
            Ok(Some(prepared)) => Some(prepared.arguments),
            Ok(None) => Some(inferred.into_iter().map_while(|arg| arg).collect()),
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
    }

    /// The same for a generic item's arguments.
    pub(crate) fn complete_item_generic_args(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: &ItemPath,
        inferred: Vec<Option<ResolvedGenericArg>>,
    ) -> Option<Vec<ResolvedGenericArg>> {
        if inferred.iter().all(Option::is_some) {
            return Some(inferred.into_iter().flatten().collect());
        }
        match self.resolver.complete_generic_args(absolute, &inferred) {
            Ok(arguments) => Some(arguments),
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                None
            }
        }
    }

    /// How a diagnostic names the declaration a generic call reached: the
    /// owner's own path with the function appended.
    pub(crate) fn owner_item_path(owner: &ResolvedType, member: &Ident) -> ItemPath {
        let owner = owner.autoderef();
        let module = owner
            .declaring_owner()
            .map(|(module_path, _)| module_path)
            .unwrap_or_else(|| ModulePath::new([]));
        module.child(Self::owner_name(owner)).join(member.clone())
    }

    /// `GenericOwner::name(...)` and `GenericOwner::self::name(receiver, ...)`
    /// where the owner's type arguments are inferred from the call. A member
    /// call infers them from its explicit receiver argument like any other.
    /// An owner list written with `_` holes (`Pair<_, u8>::new(...)`) binds
    /// what it writes and leaves the holes to the same inference.
    pub(crate) fn resolve_generic_owner_function_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        expected: Expected<'_>,
    ) -> Intercepted {
        let Some(expr_path) = Self::callee_expr_path(call) else {
            return Intercepted::Declined;
        };
        if expr_path.qualified_spec.is_some() {
            return Intercepted::Declined;
        }
        let path = &expr_path.path;
        let (namespace, member, member_segments) = match path.tail.as_slice() {
            [.., segment, member] if segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT => {
                (FunctionNamespace::Member, member, 2)
            }
            [.., member] => (FunctionNamespace::Static, member, 1),
            [] => return Intercepted::Declined,
        };
        let written = match Self::member_generics(expr_path, member_segments) {
            Some(MemberGenerics {
                owner: None,
                function: None,
            }) => Vec::new(),
            Some(MemberGenerics {
                owner: Some(owner),
                function: None,
            }) => match Self::plain_generic_args(owner) {
                Some(written) if written.contains(&GenericArg::Infer) => written,
                _ => return Intercepted::Declined,
            },
            _ => return Intercepted::Declined,
        };
        let mut owner_path = path.clone();
        owner_path.tail.truncate(path.tail.len() - member_segments);
        if owner_path.is_unqualified()
            && owner_path.anchor.is_none()
            && self.context.find_defined_type(&path.head).is_some()
        {
            return Intercepted::Declined;
        }

        // Every spelling of the owner -- an import, an alias, a module path,
        // an anchor -- reaches the same absolute item path a type position
        // would.
        let accessor = self.path_module(path);
        let Ok(access) =
            self.context
                .resolve_absolute_item_path(&mut *self.resolver, &owner_path, &accessor)
        else {
            return Intercepted::Declined;
        };
        let absolute = access.absolute.clone();
        let owner_segments = owner_path.segments();

        let Some((real_absolute, sig)) = self.generic_owner_function_signature_with_ambient(
            &accessor,
            &owner_segments,
            &absolute,
            member,
            namespace,
        ) else {
            return Intercepted::Declined;
        };
        if !sig.function_params().is_empty() {
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
            &owner_segments,
            &owner,
            member,
            namespace,
            &sig,
            &written,
            expected,
            path.origin,
        ))
    }

    fn generic_owner_function_signature_with_ambient(
        &mut self,
        accessor: &ModulePath,
        prefix: &[Ident],
        absolute: &ItemPath,
        function_name: &Ident,
        namespace: FunctionNamespace,
    ) -> Option<(ItemPath, GenericOwnerFunctionSignature)> {
        if let Ok(Some(sig)) =
            self.resolver
                .generic_owner_function_signature(absolute, function_name, namespace)
        {
            return Some((absolute.clone(), sig));
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

    #[allow(clippy::too_many_arguments)]
    fn finish_generic_owner_function_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        accessor: &ModulePath,
        prefix: &[Ident],
        owner: &ItemAccess,
        member: &Ident,
        namespace: FunctionNamespace,
        sig: &GenericOwnerFunctionSignature,
        written: &[GenericArg],
        expected: Expected<'_>,
        origin: Origin,
    ) -> Option<CheckedExprNode> {
        self.check_generic_arity(
            node_id,
            span,
            &owner.absolute,
            sig.owner_params(),
            written.len(),
        )?;
        let seed = self.written_hole_seed(node_id, span, written, sig.owner_params())?;
        let template = &sig.template;
        let enclosing = GenericSubstitution::new();
        let mut inference = Inference::of_template(template, &enclosing, seed);
        inference.seed_from_expected(&template.return_type, expected);
        let checked_args = self.infer_arguments(&mut inference, &template.params, &call.args)?;

        let generic_args = match Self::solved_generic_args(&inference) {
            Ok(generic_args) => generic_args[..sig.owner_generics].to_vec(),
            Err(_) => {
                if let Some((parameter, found)) =
                    inference.fat_pointer_mismatch(&template.params, &checked_args)
                {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::GenericParamFromFatPointer { parameter, found },
                    );
                } else {
                    let missing = sig
                        .owner_params()
                        .iter()
                        .zip(&inference.bindings)
                        .filter(|(param, binding)| param.default.is_none() && binding.is_none())
                        .map(|(param, _)| param.label())
                        .collect();
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedLiteralGeneric {
                            r#type: owner.absolute.name.clone(),
                            generics: missing,
                        },
                    );
                }
                return None;
            }
        };

        let generic_args =
            self.complete_item_generic_args(node_id, span, &owner.absolute, generic_args)?;
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

        if !self.check_visibility(method.visibility, &method.declaring_module, origin) {
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
        expected: Expected<'_>,
    ) -> Intercepted {
        let Some(expr_path) = Self::callee_expr_path(call) else {
            return Intercepted::Declined;
        };
        let path = &expr_path.path;
        // Generic arguments written on an earlier segment name a generic
        // owner, not this function; that stays with generic-place resolution.
        let Some(written) = Self::trailing_generic_args(expr_path) else {
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
                _ => ItemAccess::gated(accessor.join(path.head.clone())),
            }
        } else {
            match self.module_qualified_path(node_id, span, path) {
                ModuleQualifiedPath::Item(access) => access,
                ModuleQualifiedPath::NotModule => return Intercepted::Declined,
                ModuleQualifiedPath::Failed => return Intercepted::Claimed(None),
            }
        };

        // A declaration whose signature cannot be read reports why where it
        // is declared; this use only says it cannot proceed.
        let sig = match self.resolver.generic_function_signature(&access.absolute) {
            Ok(Some(sig)) => sig,
            Ok(None) => return Intercepted::Declined,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return Intercepted::Claimed(None);
            }
        };

        let explicit = match self.resolve_written_generics(
            node_id,
            span,
            written,
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
        accessor: &ModulePath,
        access: &ItemAccess,
        sig: &OverloadTemplate,
        explicit: &WrittenGenerics,
        expected: Expected<'_>,
    ) -> Option<CheckedExprNode> {
        // Written arguments bind the declaration's generics left to right;
        // inference only ever fills what is left, including the position a
        // `spec ...` selector occupies without binding.
        let enclosing = GenericSubstitution::new();
        let mut inference = Inference::of_template(sig, &enclosing, explicit.bindings.clone());
        inference.seed_from_expected(&sig.return_type, expected);
        let (checked_args, generic_args) = self.solve_generic_call(
            node_id,
            span,
            &access.absolute.name,
            &mut inference,
            &sig.params,
            &call.args,
            explicit,
        )?;
        let name = access.absolute.name.clone();
        let target = crate::resolver::GenericCallTarget::Function(&access.absolute);
        let generic_args = self.complete_call_generic_args(node_id, span, target, generic_args)?;
        self.check_written_selectors(node_id, span, &name, target, explicit, &generic_args)?;

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
