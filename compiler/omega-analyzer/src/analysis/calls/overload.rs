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

        // A module alias wins over a type interpretation whenever both could
        // apply, so a genuine `module::function` shape is never misread as
        // `Type::function` here. Silent probe -- a real resolution failure
        // isn't this function's to report; left for whichever fallback path
        // needs this same alias to surface it.
        let accessor = self.path_module(path);
        let alias = self
            .resolver
            .resolve_import_alias(&accessor, &path.head)
            .ok()
            .flatten();
        if matches!(alias, Some(ImportTarget::Module(_))) {
            return Intercepted::Declined;
        }

        let r#type = if let Some(t) = self.context.find_defined_type(&path.head) {
            t.clone()
        } else if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
            t
        } else {
            let absolute: Vec<Ident> = self
                .module_path
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect();
            match self.resolve_item_checked(&absolute, &[], true) {
                Ok(ResolvedItem::Type(t)) => t,
                _ => return Intercepted::Declined,
            }
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

    pub(crate) fn resolve_bare_overload_candidates(
        &mut self,
        ident: &Ident,
    ) -> Option<(Vec<Ident>, OverloadCandidates)> {
        let (absolute, is_alias, import_reveal) = match self
            .resolver
            .raw_import_absolute_path(&self.module_path, ident)
        {
            Ok(Some((absolute, reveal))) => (absolute, true, reveal),
            Ok(None) => (
                self.module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(ident.clone()))
                    .collect(),
                false,
                false,
            ),
            // A raw lookup failure isn't this helper's to report -- the
            // caller's ordinary fallback path re-derives it for real.
            Err(_) => return None,
        };
        let (name, module_path) = absolute.split_last()?;
        let raw_candidates = self
            .resolver
            .function_overload_signatures(module_path, name)
            .ok()
            .flatten()?;
        let candidates = if is_alias && !import_reveal {
            raw_candidates
                .into_iter()
                .filter(|candidate| {
                    Self::visibility_allows(candidate.visibility, module_path, &self.module_path)
                })
                .collect()
        } else {
            raw_candidates
        };
        Some((absolute, candidates))
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

        // Unqualified (possibly aliased) and module-qualified names take
        // different paths: an alias's candidate set is fixed and
        // visibility-filtered at resolution time (see
        // `resolve_bare_overload_candidates`), while a module-qualified
        // reference has no alias to fix anything through, so every
        // candidate is considered and `reveal` at the call site can still
        // bypass the winner's visibility.
        let (name, module_path, candidates, needs_visibility_check): (Ident, Vec<Ident>, _, bool) =
            if path.is_unqualified() {
                let Some((absolute, candidates)) =
                    self.resolve_bare_overload_candidates(&path.head)
                else {
                    return Intercepted::Declined;
                };
                let Some((name, module_path)) = Self::split_item_path(&absolute) else {
                    return Intercepted::Declined;
                };
                (name, module_path, candidates, false)
            } else {
                let absolute: Vec<Ident> = match self.resolve_alias(&path.head).ok().flatten() {
                    Some(ImportTarget::Module(target)) => target
                        .into_iter()
                        .chain(path.tail.iter().cloned())
                        .collect(),
                    _ => return Intercepted::Declined,
                };
                let Some((name, module_path)) = Self::split_item_path(&absolute) else {
                    return Intercepted::Declined;
                };
                let candidates = match self
                    .resolver
                    .function_overload_signatures(&module_path, &name)
                {
                    Ok(Some(candidates)) => candidates,
                    Ok(None) => return Intercepted::Declined,
                    Err(e) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                        return Intercepted::Claimed(None);
                    }
                };
                (name, module_path, candidates, true)
            };

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

        if needs_visibility_check && !self.check_visibility(winner.visibility, &module_path) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                    module: module_path.clone(),
                    item: name.clone(),
                }),
            );
            return Intercepted::Claimed(None);
        }

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
