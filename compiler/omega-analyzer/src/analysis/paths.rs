use super::*;

impl<'r> Analyzer<'r> {
    pub fn resolve_gap_path(
        &mut self,
        id: HirId,
        span: Span,
        path: &Path,
    ) -> Option<std::rc::Rc<crate::resolved_type::ResolvedGap>> {
        let access = match self.context.resolve_absolute_item_path(
            &mut *self.resolver,
            path,
            &self.module_path,
        ) {
            Ok(access) => access,
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::UnresolvedType(error));
                return None;
            }
        };
        let absolute = access.absolute.clone();
        match self.resolve_item_checked(&access, &[], true) {
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
        access: ItemAccess,
        unqualified: Option<&Ident>,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        let absolute = access.absolute.clone();
        if !self.check_macro_dependency_visibility(node_id, span, written_path, &absolute) {
            return None;
        }
        // Selecting one overload as a value uses the same authorized
        // candidate set calling it does, so the two cannot disagree about
        // which candidates exist.
        if let Ok(Some(set)) = self.overload_set(accessor, &access) {
            let signatures: Vec<(HirId, ResolvedFunctionType)> = set
                .candidates
                .iter()
                .map(|candidate| (candidate.decl_id, candidate.fn_type.clone()))
                .collect();
            if let Some(ResolvedType::Function(expected_fn)) = expected
                && let Some((decl_id, fn_type)) =
                    Self::unique_overload_signature_match(expected_fn, &signatures)
            {
                let r#type = ResolvedType::Function(fn_type);
                let root = CheckedPlaceRoot::Variable {
                    decl_id,
                    storage: Storage::Function,
                    r#type: r#type.clone(),
                };
                return Some((root, r#type, false));
            }
            let name = set
                .absolute
                .last()
                .cloned()
                .expect("an overload set path always ends in the group's name");
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name,
                    candidates: set
                        .candidates
                        .into_iter()
                        .map(|candidate| candidate.fn_type)
                        .collect(),
                },
            );
            return None;
        }
        match self.resolver.resolve_item(
            accessor,
            &absolute,
            &[],
            access.options(ResolveItemOptions::INDIRECT),
        ) {
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
                match self.resolve_item_checked(&ItemAccess::gated(missing.clone()), &[], true) {
                    Ok(ResolvedItem::Type(t)) => self
                        .resolve_type_member(
                            node_id,
                            span,
                            &t,
                            &absolute[missing.len()..],
                            expected,
                        )
                        .map(|(root, r#type)| (root, r#type, false)),
                    Ok(ResolvedItem::Gap(gap)) => self
                        .resolve_gap_member(node_id, span, &gap, &absolute[missing.len()..])
                        .map(|(root, r#type)| (root, r#type, false)),
                    // The prefix genuinely doesn't resolve either: the
                    // original "no such module" reading is the right report.
                    Ok(ResolvedItem::Value { .. }) | Err(ResolveError::UnknownItem { .. }) => {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(
                                missing,
                            )),
                        );
                        None
                    }
                    // The prefix *does* name a type/gap/item, just not one
                    // this accessor may reach (or some other specific
                    // failure) -- that is the real cause, and reporting
                    // "unknown module" instead would hide it.
                    Err(e) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
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
        // An explicit anchor names the head directly, so none of the
        // unanchored lexical/import readings below apply to it.
        let anchored = match self.anchored_path(node_id, span, path) {
            AnchoredPath::Failed => return None,
            AnchoredPath::Absolute(absolute) => Some(absolute),
            AnchoredPath::Unanchored => None,
        };
        if anchored.is_none() {
            if path.head.as_ref() == "str" {
                return self.resolve_type_member(
                    node_id,
                    span,
                    &ResolvedType::Str { mutable: false },
                    &path.tail,
                    expected,
                );
            }
            if let Some(head_type) = self.context.find_defined_type(&path.head).cloned() {
                return self.resolve_type_member(node_id, span, &head_type, &path.tail, expected);
            }
        }

        let access = match anchored {
            // The anchor resolves the whole written path, so the head alone
            // is its own absolute path and `path.tail` stays the member
            // chain, exactly as for an unanchored head.
            Some(absolute) => {
                ItemAccess::gated(absolute[..absolute.len() - path.tail.len()].to_vec())
            }
            None => {
                let alias = self.resolve_path_alias_or_error(node_id, span, path)?;
                if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
                    return self.resolve_type_member(node_id, span, &t, &path.tail, expected);
                }
                if let Some(ImportTarget::Item(_, ResolvedItem::Gap(gap))) = alias {
                    return self.resolve_gap_member(node_id, span, &gap, &path.tail);
                }
                match alias {
                    Some(ImportTarget::ItemPath(access)) => access,
                    Some(ImportTarget::Module(absolute)) => ItemAccess::gated(absolute),
                    _ => ItemAccess::gated(
                        self.path_module(path)
                            .iter()
                            .cloned()
                            .chain(std::iter::once(path.head.clone()))
                            .collect(),
                    ),
                }
            }
        };
        let absolute = access.absolute.clone();
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
                    &ItemAccess {
                        absolute: real_absolute,
                        bypass_visibility: access.bypass_visibility,
                    },
                    &type_args,
                )
            }
            None => self.resolve_item_checked_with_ambient_fallback(
                std::slice::from_ref(&path.head),
                &access,
                &[],
            ),
        };
        let kind = match result {
            Ok(ResolvedItem::Type(t)) => {
                return self.resolve_type_member(node_id, span, &t, &path.tail, expected);
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
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let segments = expr_path.path.segments();
        let rest = &segments[expr_path.args_at + 1..];
        // One member segment, or the two that select the member namespace.
        let too_deep = match rest {
            [] | [_] => false,
            [first, _] => first.as_ref() != FunctionNamespace::MEMBER_SEGMENT,
            _ => true,
        };
        if too_deep {
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
        let access = self.generic_prefix_absolute(node_id, span, &expr_path.path, prefix)?;
        let absolute = access.absolute.clone();
        let accessor = self.path_module(&expr_path.path);
        match self.resolve_item_with_ambient_from(&accessor, prefix, &access, &type_args) {
            Ok(ResolvedItem::Type(_)) if rest.is_empty() => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Ok(ResolvedItem::Type(t)) => {
                self.resolve_type_member(node_id, span, &t, rest, expected)
            }
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
    ) -> Option<ItemAccess> {
        let module = self.path_module(written_path);
        if let [single] = prefix
            && written_path.anchor.is_none()
        {
            if let Some(ImportTarget::ItemPath(access)) =
                match self.resolver.resolve_import_alias(&module, single) {
                    Ok(alias) => Some(alias),
                    Err(error) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                        None
                    }
                }?
            {
                return Some(access);
            }
            return Some(ItemAccess::gated(
                module
                    .iter()
                    .cloned()
                    .chain(std::iter::once(single.clone()))
                    .collect(),
            ));
        }

        let Some((head, tail)) = prefix.split_first() else {
            return None;
        };
        let prefix_path = Path {
            anchor: written_path.anchor,
            head: head.clone(),
            tail: tail.to_vec(),
            origin: written_path.origin,
        };
        match self.module_qualified_path(node_id, span, &prefix_path) {
            ModuleQualifiedPath::Item(access) => Some(access),
            ModuleQualifiedPath::Failed => None,
            ModuleQualifiedPath::NotModule => {
                let similar_module =
                    best_match(head, self.resolver.import_alias_names(&module).iter());
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

    /// The namespace a type-qualified tail selects, the function it names,
    /// and anything written after that function.
    ///
    /// `self` is contextual here: it only opens the member namespace
    /// directly after a resolved type and only when a further segment
    /// follows, so `Type::self` still names a static function or enum
    /// variant literally called `self`, and a leading module-relative
    /// `self::...` is never reinterpreted.
    fn select_function_namespace(rest: &[Ident]) -> (FunctionNamespace, &Ident, &[Ident]) {
        if let [segment, function, deeper @ ..] = rest
            && segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT
        {
            return (FunctionNamespace::Member, function, deeper);
        }
        let (function, deeper) = rest
            .split_first()
            .expect("a type-qualified path always names at least one member");
        (FunctionNamespace::Static, function, deeper)
    }

    /// The declaration-owned functions of a concrete owner, together with
    /// what a diagnostic needs to name it. `None` means the type owns no
    /// function declarations at all and the error was already reported.
    fn owner_functions(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: &ResolvedType,
    ) -> Option<OwnerFunctions> {
        match r#type {
            ResolvedType::Struct(cell) => {
                let owner = cell.borrow();
                Some(OwnerFunctions {
                    name: owner.name.clone(),
                    functions: owner.functions.clone(),
                    module_path: owner.module_path.clone(),
                    id: owner.id,
                    variants: None,
                })
            }
            ResolvedType::Union(cell) => {
                let owner = cell.borrow();
                Some(OwnerFunctions {
                    name: owner.name.clone(),
                    functions: owner.functions.clone(),
                    module_path: owner.module_path.clone(),
                    id: owner.id,
                    variants: None,
                })
            }
            ResolvedType::Enum { cell, .. } => {
                let owner = cell.borrow();
                Some(OwnerFunctions {
                    name: owner.name.clone(),
                    functions: owner.functions.clone(),
                    module_path: owner.module_path.clone(),
                    id: owner.id,
                    variants: Some(owner.variants.iter().map(|v| v.name.clone()).collect()),
                })
            }
            other => {
                let functions = match self.resolver.primitive_methods(other) {
                    Ok(functions) => functions,
                    Err(err) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                        return None;
                    }
                };
                if functions.is_empty() {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::StaticAccessOnNonStruct {
                            found: other.clone(),
                        },
                    );
                    return None;
                }
                Some(OwnerFunctions {
                    name: Ident(other.to_string()),
                    functions,
                    module_path: Vec::new(),
                    id: node_id,
                    variants: None,
                })
            }
        }
    }

    fn resolve_type_member(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: &ResolvedType,
        rest: &[Ident],
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let (namespace, member, deeper) = Self::select_function_namespace(rest);

        // A variant is an ordinary member of its enum's own name, never a
        // function, so `Enum::self::...` cannot reach one.
        if namespace == FunctionNamespace::Static
            && let ResolvedType::Enum { cell, .. } = r#type
        {
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
        }

        let owner = self.owner_functions(node_id, span, r#type)?;
        let mut owner_module_path = owner.module_path.clone();
        let mut owner_id = owner.id;
        let mut candidates = namespace.select(&owner.functions, member);

        // An inherent declaration wins over a conforming one, but only
        // inside the selected namespace: an inherent static must not hide a
        // conforming member from `Type::self::name`, or the reverse.
        let mut conformances = Vec::new();
        if candidates.is_empty() {
            conformances = match self.resolver.conformances_for_type(r#type) {
                Ok(conformances) => conformances,
                Err(err) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                    return None;
                }
            };
            let providers: Vec<_> = conformances
                .iter()
                .flat_map(|conform| {
                    namespace
                        .select(&conform.methods, member)
                        .into_iter()
                        .map(move |method| (conform, method))
                })
                .collect();
            if providers.len() > 1 {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::AmbiguousConformanceFunction {
                        target: r#type.to_string(),
                        function: member.clone(),
                        specs: providers
                            .iter()
                            .map(|(conform, _)| conform.spec.borrow().name.clone())
                            .collect(),
                        namespace,
                    },
                );
                return None;
            }
            if let Some((conform, method)) = providers.into_iter().next() {
                let spec = conform.spec.borrow();
                owner_module_path = spec.module_path.clone();
                owner_id = spec.id;
                candidates = vec![method];
            }
        }

        if candidates.is_empty() {
            let sibling = namespace.other();
            let has_sibling = !sibling.select(&owner.functions, member).is_empty()
                || conformances
                    .iter()
                    .any(|conform| !sibling.select(&conform.methods, member).is_empty());
            let kind = if has_sibling {
                AnalysisErrorKind::FunctionNamespaceMismatch {
                    owner: owner.name.clone(),
                    function: member.clone(),
                    declared_in: sibling,
                }
            } else {
                owner.missing(member, namespace)
            };
            self.error(node_id, span, kind);
            return None;
        }

        if !deeper.is_empty() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::StructPathTooDeep {
                    r#struct: owner.name.clone(),
                    function: member.clone(),
                    namespace,
                },
            );
            return None;
        }

        let method = self.select_uncalled_function(node_id, span, member, candidates, expected)?;
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

        let fn_type = ResolvedType::Function(method.value_fn_type());
        let root = CheckedPlaceRoot::Variable {
            decl_id: method.decl_id,
            storage: Storage::Function,
            r#type: fn_type.clone(),
        };
        Some((root, fn_type))
    }

    /// Picks the one candidate an uncalled reference names. Overloads within
    /// a namespace are separated by the expected ordinary function type,
    /// which for a member is its unbound explicit-receiver view.
    fn select_uncalled_function(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: Vec<ResolvedMethod>,
        expected: Option<&ResolvedType>,
    ) -> Option<ResolvedMethod> {
        if let [only] = candidates.as_slice() {
            return Some(only.clone());
        }
        let signatures: Vec<(HirId, ResolvedFunctionType)> = candidates
            .iter()
            .map(|method| (method.decl_id, method.value_fn_type()))
            .collect();
        if let Some(ResolvedType::Function(expected_fn)) = expected
            && let Some((decl_id, _)) =
                Self::unique_overload_signature_match(expected_fn, &signatures)
        {
            return candidates
                .into_iter()
                .find(|method| method.decl_id == decl_id);
        }
        self.error(
            node_id,
            span,
            AnalysisErrorKind::AmbiguousOverload {
                name: name.clone(),
                candidates: signatures.into_iter().map(|(_, sig)| sig).collect(),
            },
        );
        None
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
            .map(|field| field.name.clone())
            .collect();
        if !dynamic_field_names.is_empty() || !variant.fields.is_empty() {
            let fields = dynamic_field_names
                .into_iter()
                .chain(variant.fields.iter().map(|field| field.name.clone()))
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

/// A concrete owner's own function declarations, plus what a namespace
/// diagnostic needs to name it. Enum variants are carried alongside because
/// they share the ordinary namespace with static functions.
struct OwnerFunctions {
    name: Ident,
    functions: Vec<(Ident, ResolvedMethod)>,
    module_path: Vec<Ident>,
    id: HirId,
    variants: Option<Vec<Ident>>,
}

impl OwnerFunctions {
    fn missing(&self, member: &Ident, namespace: FunctionNamespace) -> AnalysisErrorKind {
        let similar = best_match(member, namespace.names(&self.functions).into_iter());
        match &self.variants {
            Some(variants) if namespace == FunctionNamespace::Static => {
                AnalysisErrorKind::NoSuchEnumMember {
                    r#enum: self.name.clone(),
                    name: member.clone(),
                    similar_variant: best_match(member, variants.iter()),
                    similar_function: similar,
                }
            }
            _ => AnalysisErrorKind::NoSuchStructFunction {
                r#struct: self.name.clone(),
                function: member.clone(),
                similar,
            },
        }
    }
}
