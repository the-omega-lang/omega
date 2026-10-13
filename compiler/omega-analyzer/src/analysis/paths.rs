use super::*;

impl<'r> Analyzer<'r> {
    pub fn resolve_gap_path(
        &mut self,
        id: HirId,
        span: Span,
        path: &Path,
    ) -> Option<std::rc::Rc<crate::resolved_type::ResolvedGap>> {
        let accessor = self.path_module(path);
        let access =
            match self
                .context
                .resolve_absolute_item_path(&mut *self.resolver, path, &accessor)
            {
                Ok(access) => access,
                Err(error) => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(error));
                    return None;
                }
            };
        let absolute = access.absolute.clone();
        match self.resolve_item_checked(&access, &[], true, path.origin) {
            Ok(ResolvedItem::Gap(gap)) => Some(gap),
            Ok(_) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::GlueTargetNotGap {
                        target: absolute.name.clone(),
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
        accessor: &ModulePath,
        access: ItemAccess,
        unqualified: Option<&Ident>,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        let absolute = access.absolute.clone();
        // Selecting one declaration as a value uses the same authorized
        // candidate set calling it does, so the two cannot disagree about
        // which declarations exist.
        if let Ok(Some(set)) = self.function_value_set(accessor, &access, written_path.origin)
            && !set.candidates.is_empty()
        {
            match self.select_function_value(
                node_id,
                span,
                &set.absolute.name,
                &set.absolute,
                &set.candidates,
                &[],
                expected,
            ) {
                FunctionValue::Selected {
                    decl_id, fn_type, ..
                } => {
                    let r#type = ResolvedType::Function(fn_type);
                    let root = CheckedPlaceRoot::Variable {
                        decl_id,
                        storage: Storage::Function,
                        r#type: r#type.clone(),
                    };
                    return Some((root, r#type, false));
                }
                FunctionValue::Failed => return None,
                // A lone generic declaration this reference cannot
                // instantiate: the ordinary item query reports it, naming the
                // arguments it is missing.
                FunctionValue::Undetermined => {}
            }
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
            Err(ResolveError::UnknownModule(missing)) if missing == absolute.module => {
                let Some(prefix) = missing.as_item() else {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(missing)),
                    );
                    return None;
                };
                match self.resolve_item_checked(
                    &ItemAccess::gated(prefix),
                    &[],
                    true,
                    written_path.origin,
                ) {
                    Ok(ResolvedItem::Type(t)) => self
                        .resolve_type_member(
                            node_id,
                            span,
                            &t,
                            std::slice::from_ref(&absolute.name),
                            &[],
                            expected,
                            written_path.origin,
                        )
                        .map(|(root, r#type)| (root, r#type, false)),
                    Ok(ResolvedItem::Gap(gap)) => self
                        .resolve_gap_member(
                            node_id,
                            span,
                            written_path,
                            &gap,
                            std::slice::from_ref(&absolute.name),
                        )
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
                    &[],
                    expected,
                    path.origin,
                );
            }
            if let Some(head_type) = self.context.find_defined_type(&path.head).cloned() {
                return self.resolve_type_member(
                    node_id,
                    span,
                    &head_type,
                    &path.tail,
                    &[],
                    expected,
                    path.origin,
                );
            }
        }

        let access = match anchored {
            // The anchor resolves the whole written path, so the head alone
            // is its own absolute path and `path.tail` stays the member
            // chain, exactly as for an unanchored head.
            Some(absolute) => ItemAccess::gated(
                ItemPath::from_segments(&absolute[..absolute.len() - path.tail.len()])
                    .expect("an anchored path keeps its head"),
            ),
            None => {
                let alias = self.resolve_path_alias_or_error(node_id, span, path)?;
                if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
                    return self.resolve_type_member(
                        node_id,
                        span,
                        &t,
                        &path.tail,
                        &[],
                        expected,
                        path.origin,
                    );
                }
                if let Some(ImportTarget::Item(_, ResolvedItem::Gap(gap))) = alias {
                    return self.resolve_gap_member(node_id, span, path, &gap, &path.tail);
                }
                match alias {
                    Some(ImportTarget::ItemPath(access)) => access,
                    Some(ImportTarget::Module(module)) => ItemAccess::gated(
                        module
                            .as_item()
                            .expect("an imported module is never the root"),
                    ),
                    _ => ItemAccess::gated(self.path_module(path).join(path.head.clone())),
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
                let generic_args = self.infer_literal_type_args(
                    node_id,
                    span,
                    &real_absolute,
                    &sig,
                    &[],
                    expected.into(),
                    Vec::new(),
                )?;
                self.resolve_item_checked_with_ambient_fallback(
                    std::slice::from_ref(&path.head),
                    &ItemAccess {
                        absolute: real_absolute,
                        bypass_visibility: access.bypass_visibility,
                    },
                    &generic_args,
                    path.origin,
                )
            }
            None => self.resolve_item_checked_with_ambient_fallback(
                std::slice::from_ref(&path.head),
                &access,
                &[],
                path.origin,
            ),
        };
        let kind = match result {
            Ok(ResolvedItem::Type(t)) => {
                return self.resolve_type_member(
                    node_id,
                    span,
                    &t,
                    &path.tail,
                    &[],
                    expected,
                    path.origin,
                );
            }
            Ok(ResolvedItem::Gap(gap)) => {
                return self.resolve_gap_member(node_id, span, path, &gap, &path.tail);
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
        if expr_path.generic_args.len() > 1 {
            return self.resolve_owner_and_function_value(node_id, span, expr_path, expected);
        }
        let segments = expr_path.path.segments();
        let (args_at, written) = self.single_generic_list(node_id, span, expr_path)?;
        let rest = &segments[args_at + 1..];
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
                    r#type: segments[args_at].clone(),
                },
            );
            return None;
        }

        // With nothing after them, the written arguments belong to the last
        // segment, which may be a function rather than an item prefix.
        if rest.is_empty() {
            if let Some(selected) =
                self.resolve_explicit_function_value(node_id, span, expr_path, written, expected)
            {
                return selected;
            }
            // No generic function takes the list, so after an item it is on
            // a member -- a variant or a non-generic function.
            if args_at > 0 && self.prefix_names_item(node_id, span, &expr_path.path, args_at - 1) {
                self.report_misplaced_generic_args(node_id, span, &segments, args_at);
                return None;
            }
        }

        let prefix = &segments[..=args_at];
        let access = self.generic_prefix_absolute(node_id, span, &expr_path.path, prefix)?;
        let absolute = access.absolute.clone();
        let accessor = self.path_module(&expr_path.path);
        let params = self.item_generic_params_for(&accessor, prefix, &access);
        let written = self.ordinary_generic_args(
            node_id,
            span,
            written,
            "a path that does not name a generic function",
        )?;
        let hole_literal = match rest {
            [variant] if written.contains(&GenericArg::Infer) => {
                self.generic_literal_signature_with_ambient(prefix, &absolute, Some(variant))
            }
            _ => None,
        };
        let generic_args = match hole_literal {
            Some((real_absolute, sig)) => {
                self.check_generic_arity(node_id, span, &absolute, &params, written.len())?;
                let seed = self.written_hole_seed(node_id, span, &written, &sig.generics)?;
                self.infer_literal_type_args(
                    node_id,
                    span,
                    &real_absolute,
                    &sig,
                    &[],
                    expected.into(),
                    seed,
                )?
            }
            None => {
                self.resolve_generic_arg_list(node_id, span, &written, &access.absolute, &params)?
            }
        };
        match self.resolve_item_with_ambient_from(&accessor, prefix, &access, &generic_args) {
            Ok(ResolvedItem::Type(_)) if rest.is_empty() => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Ok(ResolvedItem::Type(t)) => self.resolve_type_member(
                node_id,
                span,
                &t,
                rest,
                &[],
                expected,
                expr_path.path.origin,
            ),
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
                        name: segments[args_at].clone(),
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

    /// `f<A>` and `Owner::name<A>` as values: a reference whose generic
    /// arguments belong to the function itself.
    ///
    /// `None` declines, leaving the ordinary item reading -- and its
    /// diagnostics -- in place. Both readings are tried because the same
    /// spelling can be either: `module::f<A>` names an item under a module,
    /// while `Owner::f<A>` names a function of a type. A reference is claimed
    /// only when a generic declaration exists to select, so writing arguments
    /// on a name that has none still reports the ordinary arity error.
    fn resolve_explicit_function_value(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
        explicit: &[ExprGenericArg],
        expected: Option<&ResolvedType>,
    ) -> Option<Option<(CheckedPlaceRoot, ResolvedType)>> {
        let path = &expr_path.path;
        // The `self` segment belongs to the owner-to-function path, not to
        // the owner itself, exactly as it does at a call.
        let member = match path.tail.as_slice() {
            [.., segment, member] if segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT => {
                Some((FunctionNamespace::Member, member, 2))
            }
            [.., member] => Some((FunctionNamespace::Static, member, 1)),
            [] => None,
        };
        if let Some((namespace, member, member_segments)) = member
            && let Some(owner) =
                self.callee_owner_type(node_id, span, expr_path, member_segments, None)
        {
            let templates = self
                .resolver
                .generic_method_candidates(&owner, member, namespace)
                .and_then(|inherent| {
                    if !inherent.is_empty() {
                        return Ok(inherent);
                    }
                    self.resolver
                        .conformance_generic_method_candidates(&owner, member, namespace)
                });
            match templates {
                Ok(templates) if templates.is_empty() => return None,
                Ok(_) => {}
                Err(error) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                    return Some(None);
                }
            }
            let rest = &path.tail[path.tail.len() - member_segments..];
            return Some(self.resolve_type_member(
                node_id,
                span,
                &owner,
                rest,
                explicit,
                expected,
                path.origin,
            ));
        }

        let segments = path.segments();
        let accessor = self.path_module(path);
        let access = self.without_diagnostics(|this| {
            this.generic_prefix_absolute(node_id, span, path, &segments)
        })?;
        let set = self
            .function_value_set(&accessor, &access, path.origin)
            .ok()
            .flatten()?;
        if !set.candidates.iter().any(|c| c.template().is_some()) {
            return None;
        }
        let name = set.absolute.name.clone();
        Some(
            match self.select_function_value(
                node_id,
                span,
                &name,
                &set.absolute,
                &set.candidates,
                explicit,
                expected,
            ) {
                FunctionValue::Selected {
                    decl_id, fn_type, ..
                } => {
                    let r#type = ResolvedType::Function(fn_type);
                    let root = CheckedPlaceRoot::Variable {
                        decl_id,
                        storage: Storage::Function,
                        r#type: r#type.clone(),
                    };
                    Some((root, r#type))
                }
                FunctionValue::Failed => None,
                FunctionValue::Undetermined => return None,
            },
        )
    }

    /// The one generic list a path writes, and the segment it follows. Only a
    /// type-qualified function path takes two, so a second list, or a list
    /// on a `self` namespace segment, is reported as misplaced here.
    pub(super) fn single_generic_list<'p>(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &'p ExprPath,
    ) -> Option<(usize, &'p [ExprGenericArg])> {
        let segments = expr_path.path.segments();
        let mut lists = expr_path.generic_args.iter();
        let (at, written) = lists.next()?;
        let misplaced = match lists.next() {
            Some((second, _)) => Some(second),
            None => (segments[at].as_ref() == FunctionNamespace::MEMBER_SEGMENT
                || self.prefix_names_module(&expr_path.path, at))
            .then_some(at),
        };
        if let Some(misplaced) = misplaced {
            self.report_misplaced_generic_args(node_id, span, &segments, misplaced);
            return None;
        }
        Some((at, written))
    }

    /// Whether `path` up to and including segment `at` names a module, which
    /// never takes generic arguments. Silent: any other reading reports its
    /// own errors.
    fn prefix_names_module(&mut self, path: &Path, at: usize) -> bool {
        let mut prefix = path.clone();
        prefix.tail.truncate(at);
        let accessor = self.path_module(path);
        let absolute = match self.resolver.resolve_explicit_anchor(&accessor, &prefix) {
            Some(Ok(absolute)) => absolute,
            Some(Err(_)) => return false,
            None => match self.resolver.resolve_import_alias(&accessor, &prefix.head) {
                Ok(Some(ImportTarget::Module(target))) => target
                    .segments()
                    .iter()
                    .cloned()
                    .chain(prefix.tail)
                    .collect(),
                _ => return false,
            },
        };
        matches!(
            self.resolver
                .resolve_module_path(&accessor, &ModulePath::from(absolute)),
            Ok(Some(_))
        )
    }

    /// Whether `path` up to and including segment `at` names an item. Silent,
    /// like [`Self::prefix_names_module`].
    fn prefix_names_item(&mut self, node_id: HirId, span: Span, path: &Path, at: usize) -> bool {
        let segments = path.segments();
        let prefix = &segments[..=at];
        let Some(access) = self
            .without_diagnostics(|this| this.generic_prefix_absolute(node_id, span, path, prefix))
        else {
            return false;
        };
        if matches!(
            self.resolver.item_generic_params(&access.absolute),
            Ok(Some(_))
        ) {
            return true;
        }
        let [single] = prefix else { return false };
        let accessor = self.path_module(path);
        matches!(
            self.resolver.ambient_core_candidates(&accessor, single),
            Ok(Some(_))
        )
    }

    fn report_misplaced_generic_args(
        &mut self,
        node_id: HirId,
        span: Span,
        segments: &[Ident],
        at: usize,
    ) {
        self.error(
            node_id,
            span,
            AnalysisErrorKind::MisplacedGenericArgs {
                segment: segments[at].clone(),
            },
        );
    }

    /// `Owner<A>::name<B>` and `Owner<A>::self::name<B>` as values: the only
    /// paths that write two lists, one on the owner and one on its function.
    fn resolve_owner_and_function_value(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let path = &expr_path.path;
        let segments = path.segments();
        let member_segments = match path.tail.as_slice() {
            [.., segment, _] if segment.as_ref() == FunctionNamespace::MEMBER_SEGMENT => 2,
            _ => 1,
        };
        let function_at = path.tail.len();
        let owner_at = function_at.saturating_sub(member_segments);
        let lists = match Self::member_generics(expr_path, member_segments) {
            Some(lists)
                if lists.owner.is_some()
                    && lists.function.is_some()
                    && !self.prefix_names_module(path, owner_at) =>
            {
                lists
            }
            _ => {
                let misplaced = expr_path
                    .generic_args
                    .iter()
                    .map(|(at, _)| at)
                    .find(|at| *at != owner_at && *at != function_at)
                    .unwrap_or(owner_at);
                self.report_misplaced_generic_args(node_id, span, &segments, misplaced);
                return None;
            }
        };
        let owner =
            self.owner_type_with_generics(node_id, span, expr_path, member_segments, &lists)?;
        let rest = &path.tail[path.tail.len() - member_segments..];
        self.resolve_type_member(
            node_id,
            span,
            &owner,
            rest,
            lists.function.unwrap_or_default(),
            expected,
            path.origin,
        )
    }

    /// Resolves a written generic argument list against the declared
    /// parameters of the item it applies to, so each position is read as the
    /// kind that parameter binds.
    ///
    /// A surplus argument is reported here rather than at the item query:
    /// without a declared parameter there is no kind to read it as, so it
    /// cannot be resolved at all.
    pub(super) fn resolve_generic_arg_list(
        &mut self,
        node_id: HirId,
        span: Span,
        written: &[GenericArg],
        owner: &ItemPath,
        params: &[GenericParam],
    ) -> Option<Vec<ResolvedGenericArg>> {
        self.check_generic_arity(node_id, span, owner, params, written.len())?;
        let args: Vec<(usize, GenericArg)> = written.iter().cloned().enumerate().collect();
        self.analyze_all(&args, |this, (index, arg)| {
            this.resolve_generic_arg_or_error(node_id, span, arg, params.get(*index))
        })
    }

    /// What a written list with `_` holes binds, positionally, as a seed for
    /// inference. Each hole leaves its position open, exactly as an omitted
    /// argument would.
    pub(super) fn written_hole_seed(
        &mut self,
        node_id: HirId,
        span: Span,
        written: &[GenericArg],
        params: &[GenericParam],
    ) -> Option<Vec<Option<ResolvedGenericArg>>> {
        let mut seed = vec![None; params.len()];
        let mut ok = true;
        for (position, (param, arg)) in params.iter().zip(written).enumerate() {
            if *arg == GenericArg::Infer {
                continue;
            }
            match self.resolve_generic_arg_or_error(node_id, span, arg, Some(param)) {
                Some(resolved) => seed[position] = Some(resolved),
                None => ok = false,
            }
        }
        ok.then_some(seed)
    }

    /// `None` when more arguments were written than the declaration accepts.
    /// An empty parameter list means the declaration was not found, so the
    /// item query stays the reporter.
    pub(super) fn check_generic_arity(
        &mut self,
        node_id: HirId,
        span: Span,
        owner: &ItemPath,
        params: &[GenericParam],
        found: usize,
    ) -> Option<()> {
        if params.is_empty() || found <= params.len() {
            return Some(());
        }
        self.error(
            node_id,
            span,
            AnalysisErrorKind::ModuleResolution(ResolveError::GenericArgCountMismatch {
                module: owner.module.clone(),
                item: owner.name.clone(),
                expected: params.len(),
                found,
            }),
        );
        None
    }

    pub(super) fn resolve_generic_arg_or_error(
        &mut self,
        node_id: HirId,
        span: Span,
        arg: &GenericArg,
        param: Option<&GenericParam>,
    ) -> Option<ResolvedGenericArg> {
        self.resolve_generic_arg_validated(node_id, span, arg, param)
            .1
    }

    /// The same, also reporting whether every alias-owned obligation the
    /// argument applies was met. A caller that owns those obligations -- the
    /// type written in a bound selector -- must stop when they were not;
    /// ordinary argument resolution keeps its reported error and carries on
    /// with the expansion.
    pub(super) fn resolve_generic_arg_validated(
        &mut self,
        node_id: HirId,
        span: Span,
        arg: &GenericArg,
        param: Option<&GenericParam>,
    ) -> (bool, Option<ResolvedGenericArg>) {
        let mut valid = true;
        if let GenericArg::Type(written) = arg {
            let module = self.module_path.clone();
            valid = self.check_alias_generic_bounds(node_id, span, written, &module);
        }
        (
            valid,
            self.resolve_generic_arg_inner(node_id, span, arg, param),
        )
    }

    fn resolve_generic_arg_inner(
        &mut self,
        node_id: HirId,
        span: Span,
        arg: &GenericArg,
        param: Option<&GenericParam>,
    ) -> Option<ResolvedGenericArg> {
        let reveals = &self.reveals;
        match self.context.resolve_generic_arg(
            arg,
            param,
            &mut *self.resolver,
            &self.module_path,
            ResolveItemOptions::INDIRECT,
            &|origin| reveals.allows(origin),
        ) {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                self.error(node_id, span, AnalysisErrorKind::UnresolvedType(err));
                None
            }
        }
    }

    /// The declared generic parameters of the item a written prefix names,
    /// including the ambient-`core` fallback a bare name may reach. An
    /// unresolvable item yields no parameters: the argument list then
    /// resolves as types and the item query reports the real problem.
    pub(super) fn item_generic_params_for(
        &mut self,
        accessor: &ModulePath,
        prefix: &[Ident],
        access: &ItemAccess,
    ) -> Vec<GenericParam> {
        match self.resolver.item_generic_params(&access.absolute) {
            Ok(Some(params)) => return params,
            Ok(None) | Err(_) => {}
        }
        let [single] = prefix else { return Vec::new() };
        self.resolver
            .ambient_core_candidates(accessor, single)
            .ok()
            .flatten()
            .and_then(|ambient| self.resolver.item_generic_params(&ambient).ok().flatten())
            .unwrap_or_default()
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
            return Some(ItemAccess::gated(module.join(single.clone())));
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

    /// Selects one function of a gap. `written_path` is the path as the
    /// programmer spelled it, so a gap member reached from a macro body is
    /// checked against the macro's definition module, not against wherever
    /// the expansion landed.
    fn resolve_gap_member(
        &mut self,
        node_id: HirId,
        span: Span,
        written_path: &omega_parser::prelude::Path,
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
        let (visibility, decl_id, fn_type) = (
            function.visibility,
            function.decl_id,
            function.fn_type.clone(),
        );
        let accessor = self.path_module(written_path);
        if !Self::visibility_allows(visibility, &gap.module_path, &accessor)
            && !self.revealed(written_path.origin)
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                    module: gap.module_path.child(gap.name.clone()),
                    item: member.clone(),
                }),
            );
            return None;
        }
        let r#type = ResolvedType::Function(fn_type);
        Some((
            CheckedPlaceRoot::Variable {
                decl_id,
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
            ResolvedType::Struct(nominal) | ResolvedType::Union(nominal) => {
                let functions =
                    self.type_facts(node_id, span, |types| types.functions(nominal).cloned())?;
                Some(OwnerFunctions {
                    name: nominal.name.clone(),
                    functions: functions.to_vec(),
                    variants: None,
                })
            }
            ResolvedType::Enum { nominal, .. } => {
                let (facts, functions) = self.type_facts(node_id, span, |types| {
                    Ok((
                        types.enum_facts(nominal)?.clone(),
                        types.functions(nominal)?.clone(),
                    ))
                })?;
                Some(OwnerFunctions {
                    name: nominal.name.clone(),
                    functions: functions.to_vec(),
                    variants: Some(facts.variants.iter().map(|v| v.name.clone()).collect()),
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
                    variants: None,
                })
            }
        }
    }

    /// Resolves `Type::name` / `Type::self::name` as a value. `explicit` is
    /// the generic argument list written on the *function* segment, which a
    /// concrete owner supports; a list on the owner was already applied to
    /// `r#type` by the caller.
    fn resolve_type_member(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: &ResolvedType,
        rest: &[Ident],
        explicit: &[ExprGenericArg],
        expected: Option<&ResolvedType>,
        origin: Origin,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let (namespace, member, deeper) = Self::select_function_namespace(rest);

        // A variant is an ordinary member of its enum's own name, never a
        // function, so `Enum::self::...` cannot reach one.
        if namespace == FunctionNamespace::Static
            && let ResolvedType::Enum { nominal, .. } = r#type
        {
            let facts =
                self.type_facts(node_id, span, |types| types.enum_facts(nominal).cloned())?;
            if let Some((variant_index, variant)) = facts.variant(member) {
                return self.resolve_unit_variant(
                    node_id,
                    span,
                    nominal,
                    &facts,
                    variant_index,
                    &variant,
                    rest,
                );
            }
        }

        let owner = self.owner_functions(node_id, span, r#type)?;
        let mut candidates = namespace.select(&owner.functions, member);
        // A generic declaration is never among an owner's resolved functions,
        // so it has to be collected separately before "no such function" can
        // be concluded -- and it is an ordinary selection candidate here.
        let mut templates = match self
            .resolver
            .generic_method_candidates(r#type, member, namespace)
        {
            Ok(templates) => templates,
            Err(err) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                return None;
            }
        };

        // An inherent declaration, concrete or generic, wins over a
        // conforming one, but only inside the selected namespace: an inherent
        // static must not hide a conforming member from `Type::self::name`,
        // or the reverse.
        let mut conformances = Vec::new();
        if candidates.is_empty() && templates.is_empty() {
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
                            .map(|(conform, _)| conform.spec.name.clone())
                            .collect(),
                        namespace,
                    },
                );
                return None;
            }
            if let Some((_, method)) = providers.into_iter().next() {
                candidates = vec![method];
            }
            templates = match self
                .resolver
                .conformance_generic_method_candidates(r#type, member, namespace)
            {
                Ok(templates) => templates,
                Err(err) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                    return None;
                }
            };
        }

        if candidates.is_empty() && templates.is_empty() {
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

        let mut selection: Vec<OverloadCandidate> = candidates
            .iter()
            .map(|method| OverloadCandidate {
                decl_id: method.decl_id,
                signature: OverloadSignature::Concrete(method.fn_type.clone()),
                visibility: method.visibility,
                declaring_module: method.declaring_module.clone(),
            })
            .collect();
        selection.extend(templates);

        let declared = Self::owner_item_path(r#type, member);
        let selected = self.select_function_value(
            node_id, span, member, &declared, &selection, explicit, expected,
        );
        let FunctionValue::Selected {
            candidate,
            decl_id,
            fn_type,
        } = selected
        else {
            if matches!(selected, FunctionValue::Undetermined) {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::GenericFunctionNotInstantiated {
                        owner: owner.name.clone(),
                        function: member.clone(),
                        namespace,
                    },
                );
            }
            return None;
        };
        let chosen = &selection[candidate];
        if !self.check_visibility(chosen.visibility, &chosen.declaring_module, origin) {
            let base = match candidates.get(candidate) {
                Some(method) => method.visibility_owner(r#type),
                None => r#type.clone(),
            };
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base,
                },
            );
            return None;
        }

        let fn_type = ResolvedType::Function(fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id,
            storage: Storage::Function,
            r#type: fn_type.clone(),
        };
        Some((root, fn_type))
    }

    fn resolve_unit_variant(
        &mut self,
        node_id: HirId,
        span: Span,
        nominal: &NominalRef,
        facts: &EnumFacts,
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
        let dynamic_field_names: Vec<Ident> = facts
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
                    r#enum: nominal.name.clone(),
                    variant: variant.name.clone(),
                    fields,
                },
            );
            return None;
        }
        let r#type = ResolvedType::Enum {
            nominal: nominal.clone(),
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
