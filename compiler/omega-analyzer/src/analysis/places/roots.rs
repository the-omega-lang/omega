use super::*;

impl<'r> Analyzer<'r> {
    pub(super) fn resolve_place_root(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        match &place.root {
            HirPlaceRoot::Path(expr_path) if expr_path.plain().is_none() => {
                let (root, r#type) = self.resolve_generic_args_place(node_id, span, expr_path)?;
                Some((root, r#type, false))
            }
            HirPlaceRoot::Path(expr_path) if expr_path.path.is_unqualified() => self
                .resolve_unqualified_root(
                    node_id,
                    span,
                    &expr_path.path.head,
                    expr_path.path.origin,
                    expected,
                ),
            HirPlaceRoot::Path(expr_path) => {
                let path = &expr_path.path;
                let (root, r#type, mutable) = match self.module_qualified_path(node_id, span, path)
                {
                    ModuleQualifiedPath::Item(access) => self.resolve_qualified_value(
                        node_id,
                        span,
                        path,
                        &self.path_module(path),
                        access,
                        None,
                        expected,
                    )?,
                    ModuleQualifiedPath::NotModule => {
                        match self.module_headed_path(node_id, span, path)? {
                            Some(access) => self.resolve_qualified_value(
                                node_id,
                                span,
                                path,
                                &self.path_module(path),
                                access,
                                None,
                                expected,
                            )?,
                            None => {
                                let (root, r#type) = self
                                    .resolve_type_qualified_value(node_id, span, path, expected)?;
                                (root, r#type, false)
                            }
                        }
                    }
                    ModuleQualifiedPath::Failed => return None,
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

        if origin.0.is_none()
            && let Some(set) = self.resolve_bare_overload_candidates(ident)
        {
            let (root, r#type) = self.resolve_bare_overload_root(node_id, span, set, expected)?;
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
        let alias = match self
            .resolver
            .resolve_import_alias(&resolution_module, ident)
        {
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
        let (access, unqualified) = match alias {
            Some(ImportTarget::ItemPath(access)) => (access, None),
            _ => (
                ItemAccess::gated(
                    resolution_module
                        .iter()
                        .cloned()
                        .chain(std::iter::once(ident.clone()))
                        .collect(),
                ),
                Some(ident),
            ),
        };
        self.resolve_qualified_value(
            node_id,
            span,
            &Path {
                anchor: None,
                head: ident.clone(),
                tail: vec![],
                origin,
            },
            &resolution_module,
            access,
            unqualified,
            expected,
        )
    }

    fn resolve_bare_overload_root(
        &mut self,
        node_id: HirId,
        span: Span,
        set: ResolvedOverloadSet,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let signatures: Vec<(HirId, ResolvedFunctionType)> = set
            .candidates
            .iter()
            .map(|candidate| (candidate.decl_id, candidate.fn_type.clone()))
            .collect();
        let winner = match expected {
            Some(ResolvedType::Function(expected_fn)) => {
                Self::unique_overload_signature_match(expected_fn, &signatures)
            }
            _ => None,
        };
        let Some((decl_id, fn_type)) = winner else {
            let name = set
                .absolute
                .last()
                .expect("an absolute item path always ends in the item's own name");
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: set
                        .candidates
                        .iter()
                        .map(|candidate| candidate.fn_type.clone())
                        .collect(),
                },
            );
            return None;
        };
        let r#type = ResolvedType::Function(fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id,
            storage: Storage::Function,
            r#type: r#type.clone(),
        };
        Some((root, r#type))
    }
}
