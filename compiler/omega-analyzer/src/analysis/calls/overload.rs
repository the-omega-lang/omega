use super::*;

impl<'r> Analyzer<'r> {
    pub(crate) fn resolve_overloaded_static_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        _expected: Option<&ResolvedType>,
    ) -> Intercepted {
        let Some(path) = Self::callee_path(call) else {
            return Intercepted::Declined;
        };
        let [member] = path.tail.as_slice() else {
            return Intercepted::Declined;
        };

        // An explicit anchor names the owner directly; nothing below applies
        // to it. Silent probe throughout -- a real resolution failure isn't
        // this function's to report, and is left to whichever fallback path
        // needs the same name to surface it.
        let accessor = self.path_module(path);
        let anchored =
            match self.anchored_prefix(node_id, span, path, std::slice::from_ref(&path.head)) {
                AnchoredPath::Failed => return Intercepted::Claimed(None),
                AnchoredPath::Absolute(absolute) => Some(absolute),
                AnchoredPath::Unanchored => None,
            };
        if let Some(absolute) = &anchored
            && self.resolver.module_exists(absolute)
        {
            return Intercepted::Declined;
        }

        // A module alias wins over a type interpretation whenever both could
        // apply, so a genuine `module::function` shape is never misread as
        // `Type::function` here.
        let alias = match anchored {
            Some(_) => None,
            None => self
                .resolver
                .resolve_import_alias(&accessor, &path.head)
                .ok()
                .flatten(),
        };
        if matches!(alias, Some(ImportTarget::Module(_))) {
            return Intercepted::Declined;
        }

        let owner = match anchored {
            Some(absolute) => Some(ItemAccess::gated(absolute)),
            None if self.context.find_defined_type(&path.head).is_some() => None,
            None => match &alias {
                Some(ImportTarget::Item(_, ResolvedItem::Type(_))) => None,
                _ => Some(ItemAccess::gated(
                    self.module_path
                        .iter()
                        .cloned()
                        .chain(std::iter::once(path.head.clone()))
                        .collect(),
                )),
            },
        };
        let r#type = match owner {
            Some(access) => match self.resolve_item_checked(&access, &[], true) {
                Ok(ResolvedItem::Type(t)) => t,
                _ => return Intercepted::Declined,
            },
            None => match self.context.find_defined_type(&path.head) {
                Some(t) => t.clone(),
                None => match alias {
                    Some(ImportTarget::Item(_, ResolvedItem::Type(t))) => t,
                    _ => return Intercepted::Declined,
                },
            },
        };

        let Some(all_methods) = r#type.declared_methods() else {
            return Intercepted::Declined;
        };
        let statics: Vec<ResolvedMethod> = all_methods
            .into_iter()
            .filter(|(name, m)| name == member && m.fn_type.self_mode.is_none())
            .map(|(_, m)| m)
            .collect();
        if statics.len() < 2 {
            return Intercepted::Declined;
        }

        let candidates: Vec<(HirId, ResolvedFunctionType)> = statics
            .iter()
            .map(|m| (m.decl_id, m.fn_type.clone()))
            .collect();
        let Some((winner, args)) =
            self.resolve_overload(node_id, span, member, &candidates, &call.args)
        else {
            return Intercepted::Claimed(None);
        };
        let (decl_id, fn_type) = candidates[winner].clone();

        let (owner_module_path, owner_id) = r#type
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), node_id));
        if !self.check_member_visibility(statics[winner].visibility, &owner_module_path, owner_id) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: member.clone(),
                    base: r#type.clone(),
                },
            );
            return Intercepted::Claimed(None);
        }

        Intercepted::Claimed(Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            decl_id,
            Storage::Function,
            fn_type,
            args,
        )))
    }

    /// The candidate set a written overload name offers this caller. A
    /// use-site `reveal` is an explicit local bypass, so it joins whatever
    /// authorization the binding already carried; everything else about
    /// which candidates exist is the resolver's answer, not a rule
    /// reconstructed here.
    pub(crate) fn overload_set(
        &mut self,
        accessor: &[Ident],
        access: &ItemAccess,
    ) -> Result<Option<ResolvedOverloadSet>, ResolveError> {
        let revealed = self.reveals.active();
        let access = ItemAccess {
            absolute: access.absolute.clone(),
            bypass_visibility: access.bypass_visibility || revealed,
        };
        let set = self.resolver.resolve_overload_set(accessor, &access)?;
        if revealed
            && let Some(set) = &set
            && let Some((_, module)) = set.absolute.split_last()
            && set.candidates.iter().any(|candidate| {
                !Self::visibility_allows(candidate.visibility, module, &self.module_path)
            })
        {
            self.reveals.mark_used();
        }
        Ok(set)
    }

    pub(crate) fn resolve_bare_overload_candidates(
        &mut self,
        ident: &Ident,
    ) -> Option<ResolvedOverloadSet> {
        let alias = self
            .resolver
            .resolve_import_alias(&self.module_path, ident)
            .ok()
            .flatten();
        let access = match alias {
            Some(ImportTarget::ItemPath(access)) => access,
            Some(ImportTarget::Item(absolute, _)) => ItemAccess::gated(absolute),
            _ => ItemAccess::gated(
                self.module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(ident.clone()))
                    .collect(),
            ),
        };
        let accessor = self.module_path.clone();
        self.overload_set(&accessor, &access).ok().flatten()
    }

    pub(crate) fn resolve_overloaded_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        _expected: Option<&ResolvedType>,
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

        // Every spelling reaches the same already-authorized candidate set:
        // a bare (possibly aliased) name, an explicitly anchored path, and a
        // module-qualified path differ only in how the absolute path is
        // found, never in which candidates the caller may then choose
        // between.
        let set = if path.is_unqualified() {
            let Some(set) = self.resolve_bare_overload_candidates(&path.head) else {
                return Intercepted::Declined;
            };
            set
        } else {
            let accessor = self.path_module(path);
            let absolute: Vec<Ident> = match self.anchored_path(node_id, span, path) {
                AnchoredPath::Failed => return Intercepted::Claimed(None),
                AnchoredPath::Absolute(absolute) => absolute,
                AnchoredPath::Unanchored => {
                    match self
                        .resolver
                        .resolve_import_alias(&accessor, &path.head)
                        .ok()
                        .flatten()
                    {
                        Some(ImportTarget::Module(target)) => target
                            .into_iter()
                            .chain(path.tail.iter().cloned())
                            .collect(),
                        _ => return Intercepted::Declined,
                    }
                }
            };
            match self.overload_set(&accessor, &ItemAccess::gated(absolute)) {
                Ok(Some(set)) => set,
                Ok(None) => return Intercepted::Declined,
                Err(e) => {
                    self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                    return Intercepted::Claimed(None);
                }
            }
        };
        let Some((name, _)) = Self::split_item_path(&set.absolute) else {
            return Intercepted::Declined;
        };
        let candidates = set.candidates;

        let signatures: Vec<(HirId, ResolvedFunctionType)> = candidates
            .iter()
            .map(|candidate| (candidate.decl_id, candidate.fn_type.clone()))
            .collect();

        let Some((winner, args)) =
            self.resolve_overload(node_id, span, &name, &signatures, &call.args)
        else {
            return Intercepted::Claimed(None);
        };
        let winner = candidates[winner].clone();

        Intercepted::Claimed(Some(self.checked_call(
            node_id,
            span,
            &call.callee,
            winner.decl_id,
            Storage::Function,
            winner.fn_type,
            args,
        )))
    }

    pub(super) fn resolve_overload(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        candidates: &[(HirId, ResolvedFunctionType)],
        args: &[HirExprNode],
    ) -> Option<(usize, Vec<CheckedExprNode>)> {
        let mut fixed: Vec<Option<CheckedExprNode>> = Vec::with_capacity(args.len());
        for arg in args {
            fixed.push(if Self::adaptable_literal(arg) {
                None
            } else {
                Some(self.analyze_expr(arg, None)?)
            });
        }

        let mut viable: Vec<(usize, u32)> = Vec::new();
        for (i, (_, fn_type)) in candidates.iter().enumerate() {
            if fn_type.is_variadic || fn_type.params.len() != args.len() {
                continue;
            }
            let mut score = 0u32;
            let mut ok = true;
            for ((_, param_type), (arg, fixed_arg)) in
                fn_type.params.iter().zip(args.iter().zip(&fixed))
            {
                match fixed_arg {
                    Some(checked) => {
                        if !param_type.accepts(&checked.r#type) {
                            ok = false;
                            break;
                        }
                    }
                    None => match Self::literal_overload_fit(
                        arg,
                        param_type,
                        self.target.pointer_bits(),
                    ) {
                        Some(true) => {}
                        Some(false) => score += 1,
                        None => {
                            ok = false;
                            break;
                        }
                    },
                }
            }
            if ok {
                viable.push((i, score));
            }
        }

        let Some(min_score) = viable.iter().map(|&(_, s)| s).min() else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::NoMatchingOverload {
                    name: name.clone(),
                    candidates: candidates.iter().map(|(_, t)| t.clone()).collect(),
                },
            );
            return None;
        };
        let winners: Vec<usize> = viable
            .iter()
            .filter(|&&(_, s)| s == min_score)
            .map(|&(i, _)| i)
            .collect();
        let winner = match winners.as_slice() {
            [only] => *only,
            _ => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::AmbiguousOverload {
                        name: name.clone(),
                        candidates: winners.iter().map(|&i| candidates[i].1.clone()).collect(),
                    },
                );
                return None;
            }
        };

        let winner_params = &candidates[winner].1.params;
        let mut final_args = Vec::with_capacity(args.len());
        for (arg, fixed_arg) in args.iter().zip(fixed) {
            let checked = match fixed_arg {
                Some(checked) => checked,
                None => {
                    let index = final_args.len();
                    self.analyze_expr(arg, Some(&winner_params[index].1))?
                }
            };
            final_args.push(checked);
        }

        Some((winner, final_args))
    }

    fn literal_overload_fit(
        arg: &HirExprNode,
        target: &ResolvedType,
        pointer_bits: u32,
    ) -> Option<bool> {
        let n = match &arg.expr {
            HirExpr::Number(n) => n,
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => n,
                _ => return None,
            },
            _ => return None,
        };
        let target_kind = target.numeric_kind(pointer_bits)?;
        if matches!(target_kind, NumericKind::Float(_)) != n.fractional_part.is_some() {
            return None;
        }
        parse_number_literal(n, target_kind).ok()?;
        let default = if n.fractional_part.is_some() {
            ResolvedType::F64
        } else {
            ResolvedType::I32
        };
        Some(*target == default)
    }
}
