use super::*;

pub(super) struct FlattenedSpecFn {
    pub(super) name: Ident,
    pub(super) fn_type: ResolvedFunctionType,
    pub(super) return_type_bound: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>)>,
    pub(super) raw: RawSpecFunctionSig,
    pub(super) spec_id: HirId,
    pub(super) spec_name: Ident,
    pub(super) visibility: Visibility,
    pub(super) substitution: GenericSubstitution,
}

impl FlattenedSpecFn {
    /// The spec's own generic arguments: everything the requirement was
    /// substituted with except the implementing `Self` type.
    pub(super) fn spec_args(&self) -> Vec<ResolvedGenericArg> {
        self.substitution
            .iter()
            .filter(|(name, _)| name.as_ref() != "Self")
            .map(|(_, arg)| arg.clone())
            .collect()
    }
}

#[derive(Clone)]
pub struct PendingSpecMethod {
    pub id: HirId,
    pub fn_type: ResolvedFunctionType,
    pub return_type_bound: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>)>,
    pub raw: RawSpecFunctionSig,
    pub substitution: GenericSubstitution,
}

impl<'r> Analyzer<'r> {
    pub fn resolve_primitive_target(
        &mut self,
        id: HirId,
        span: Span,
        target: &Type,
    ) -> Option<ResolvedType> {
        if let Type::Named(path) = target
            && path.is_unqualified()
            && path.head.as_ref() == "str"
        {
            return Some(ResolvedType::Str { mutable: false });
        }
        if let Type::InferredArray(item) = target {
            let item = self.resolve_type_or_error(id, span, item, true)?;
            return Some(ResolvedType::Slice {
                item: Box::new(item),
                mutable: false,
            });
        }
        self.resolve_type_or_error_checked(id, span, target, true, true)
    }

    pub fn resolve_conform_target(
        &mut self,
        id: HirId,
        span: Span,
        target: &Type,
    ) -> Option<ResolvedType> {
        if let Type::Named(path) = target
            && path.is_unqualified()
            && path.head.as_ref() == "str"
        {
            return Some(ResolvedType::Str { mutable: false });
        }
        if let Type::InferredArray(item) = target {
            let item = self.resolve_type_or_error(id, span, item, true)?;
            return Some(ResolvedType::Slice {
                item: Box::new(item),
                mutable: false,
            });
        }
        if matches!(
            target,
            Type::Pointer(..)
                | Type::UnknownSizeArray(..)
                | Type::SizedArray(..)
                | Type::Function(..)
                | Type::SpecStatic(..)
        ) {
            self.error(id, span, AnalysisErrorKind::ConformTargetNotAType);
            return None;
        }
        let resolved = self.resolve_type_or_error(id, span, target, true)?;
        if matches!(resolved, ResolvedType::AnonymousEnum { .. }) {
            self.error(
                id,
                span,
                AnalysisErrorKind::AnonymousEnumConformTarget { r#enum: resolved },
            );
            return None;
        }
        if !Self::is_conformable_target(&resolved) {
            self.error(id, span, AnalysisErrorKind::ConformTargetNotAType);
            return None;
        }
        Some(resolved)
    }

    pub fn is_conformable_target(target: &ResolvedType) -> bool {
        matches!(
            target,
            ResolvedType::Bool
                | ResolvedType::Char
                | ResolvedType::I8
                | ResolvedType::I16
                | ResolvedType::I32
                | ResolvedType::I64
                | ResolvedType::ISize
                | ResolvedType::U8
                | ResolvedType::U16
                | ResolvedType::U32
                | ResolvedType::U64
                | ResolvedType::USize
                | ResolvedType::F32
                | ResolvedType::F64
                | ResolvedType::Slice { .. }
                | ResolvedType::Str { .. }
                | ResolvedType::Struct(..)
                | ResolvedType::Union(..)
                | ResolvedType::Enum { .. }
        )
    }

    pub fn check_conform_block(
        &mut self,
        id: HirId,
        span: Span,
        target: &ResolvedType,
        spec: &(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>),
        functions: &[HirFunctionDef],
        method_ids: &[HirId],
    ) -> Option<(
        Rc<RefCell<ResolvedSpecType>>,
        Vec<ResolvedGenericArg>,
        Vec<(Ident, ResolvedMethod)>,
        Vec<PendingSpecMethod>,
    )> {
        let (spec, spec_args) = spec.clone();
        let requirements = self.flatten_spec(id, span, &spec, &spec_args, target)?;
        let (signatures, _) = self.with_scope(|this| {
            this.analyze_all(functions, |this, function| {
                this.collect_function_signature(function)
            })
        });
        let signatures = signatures?;
        self.check_overload_duplicates(functions, &signatures);

        let source = ConformanceSource {
            spec: spec.clone(),
            spec_args: spec_args.clone(),
        };
        let mut methods = Vec::with_capacity(requirements.len());
        let mut pending = Vec::new();
        for requirement in requirements {
            let matching = functions.iter().zip(&signatures).zip(method_ids).find(
                |((function, (signature, _)), _)| {
                    function.name == requirement.name
                        && self.fn_satisfies_requirement(
                            id,
                            span,
                            signature,
                            &requirement.fn_type,
                            &requirement.return_type_bound,
                        )
                },
            );
            if let Some(((_function, (signature, annotations)), method_id)) = matching {
                methods.push((
                    requirement.name.clone(),
                    ResolvedMethod {
                        decl_id: *method_id,
                        fn_type: signature.clone(),
                        visibility: requirement.visibility,
                        annotations: annotations.clone(),
                        source: Some(source.clone()),
                    },
                ));
            } else if requirement.raw.default_body.is_some() {
                let minted_id = self.resolver.fresh_synthetic_id();
                methods.push((
                    requirement.name.clone(),
                    ResolvedMethod {
                        decl_id: minted_id,
                        fn_type: requirement.fn_type.clone(),
                        visibility: requirement.visibility,
                        annotations: crate::annotations::ResolvedAnnotations::default(),
                        source: Some(source.clone()),
                    },
                ));
                pending.push(PendingSpecMethod {
                    id: minted_id,
                    fn_type: requirement.fn_type,
                    return_type_bound: requirement.return_type_bound,
                    raw: requirement.raw,
                    substitution: requirement.substitution,
                });
            } else {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::MissingSpecFunction {
                        implementor: Ident(target.to_string()),
                        spec: requirement.spec_name.clone(),
                        spec_args: requirement.spec_args(),
                        function: requirement.name,
                    },
                );
            }
        }
        let spec_name = spec.borrow().name.clone();
        for (function, method_id) in functions.iter().zip(method_ids) {
            if !methods
                .iter()
                .any(|(_, method)| method.decl_id == *method_id)
            {
                self.error(
                    function.id,
                    function.span,
                    AnalysisErrorKind::ConformanceExtraFunction {
                        spec: spec_name.clone(),
                        function: function.name.clone(),
                    },
                );
            }
        }
        Some((spec, spec_args, methods, pending))
    }
    pub fn resolve_spec_reference(
        &mut self,
        id: HirId,
        span: Span,
        ty: &Type,
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>)> {
        let raw_args: Vec<GenericArg> = match ty {
            Type::Generic(_, args) => args.clone(),
            _ => vec![],
        };
        let name = match ty {
            Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
            _ => Ident("<spec>".to_string()),
        };
        // `resolve_type_or_error_raw`, not `resolve_type_or_error`: a bare
        // spec name is exactly the expected result here (unlike everywhere
        // else that resolves a type), so this deliberately bypasses the
        // wrapper's bare-spec-is-never-a-value-type check.
        let resolved = self.resolve_type_or_error_raw(id, span, ty, true)?;
        // The spec's own parameter kinds decide how its written arguments
        // are read, so they are resolved only after the spec is known.
        let params = match &resolved {
            ResolvedType::Spec(spec) => spec.borrow().generics.clone(),
            _ => Vec::new(),
        };
        let mut resolved_args = Vec::with_capacity(raw_args.len());
        let mut ok = true;
        for (index, arg) in raw_args.iter().enumerate() {
            match self.resolve_generic_arg_or_error(id, span, arg, params.get(index)) {
                Some(r) => resolved_args.push(r),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        match resolved {
            ResolvedType::Spec(spec) => Some((spec, resolved_args)),
            _ => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(name)),
                );
                None
            }
        }
    }

    pub fn resolve_spec_functions(
        &mut self,
        sp: &HirSpecDef,
    ) -> (
        Vec<(Ident, RawSpecFunctionSig)>,
        crate::annotations::ResolvedAnnotations,
    ) {
        self.check_redundant_hidden(sp.id, sp.explicit_hidden_span);
        let annotations = crate::annotations::resolve(
            self,
            sp.id,
            &sp.annotations,
            crate::annotations::ItemKind::Spec,
            false,
            false,
            crate::annotations::ManglingMode::Enabled,
        );

        let mut functions = Vec::new();
        let mut seen: HashSet<Ident> = HashSet::new();
        for f in &sp.functions {
            // Unlike every other node kind, a spec member's implicit default
            // is the spec's own visibility, not `hidden` -- so an explicit
            // `hidden` here is only redundant when the spec itself is hidden.
            if sp.visibility == Visibility::Hidden {
                self.check_redundant_hidden(f.id, f.explicit_hidden_span);
            }
            if !seen.insert(f.name.clone()) {
                self.error(
                    f.id,
                    f.name_span,
                    AnalysisErrorKind::Redeclaration {
                        name: f.name.clone(),
                        previous: None,
                    },
                );
                continue;
            }
            if f.is_variadic {
                self.error(
                    f.id,
                    f.span,
                    AnalysisErrorKind::VariadicSpecFunctionUnsatisfiable {
                        name: f.name.clone(),
                    },
                );
            }
            let by_value = matches!(
                f.self_mode,
                Some(SelfMode::Value) | Some(SelfMode::MutValue)
            );
            if by_value {
                self.error(
                    f.id,
                    f.span,
                    AnalysisErrorKind::SpecSelfMustBePointer {
                        name: f.name.clone(),
                    },
                );
            }
            functions.push((
                f.name.clone(),
                RawSpecFunctionSig {
                    decl_id: f.id,
                    name: f.name.clone(),
                    span: f.span,
                    name_span: f.name_span,
                    signature_span: f.signature_span,
                    return_type_span: f.return_type_span,
                    visibility: f.visibility,
                    self_mode: f.self_mode,
                    is_variadic: f.is_variadic,
                    params: f.params.clone(),
                    return_type: f.return_type.clone(),
                    default_body: f.body.clone(),
                },
            ));
        }
        (functions, annotations)
    }

    fn resolve_spec_application_cell(
        &mut self,
        id: HirId,
        span: Span,
        ty: &Type,
        ambient_fallback: bool,
        module: &[Ident],
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<GenericArg>)> {
        let (path, raw_args) = match ty {
            Type::Generic(path, args) => (path, args.clone()),
            Type::Named(path) => (path, vec![]),
            _ => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(Ident(
                        "<spec>".to_string(),
                    ))),
                );
                return None;
            }
        };
        let absolute =
            match self
                .context
                .resolve_absolute_item_path(&mut *self.resolver, path, module)
            {
                Ok(access) => access.absolute,
                Err(e) => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(e));
                    return None;
                }
            };
        let primary = match self.resolver.spec_declaration(&absolute) {
            Ok(found) => found,
            Err(e) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(e));
                return None;
            }
        };
        let not_a_spec = || TypeResolutionError::NotASpec(path.head.clone());
        let cell = if let Some(cell) = primary {
            cell
        } else if ambient_fallback && path.is_unqualified() {
            let ambient_path = match self.resolver.ambient_core_candidates(module, &path.head) {
                Ok(Some(ambient_path)) => ambient_path,
                Ok(None) => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(not_a_spec()));
                    return None;
                }
                Err(e) => {
                    self.error(id, span, AnalysisErrorKind::ModuleResolution(e));
                    return None;
                }
            };
            match self.resolver.spec_declaration(&ambient_path) {
                Ok(Some(cell)) => cell,
                _ => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(not_a_spec()));
                    return None;
                }
            }
        } else {
            self.error(id, span, AnalysisErrorKind::UnresolvedType(not_a_spec()));
            return None;
        };
        let (visibility, declaring_module) = {
            let c = cell.borrow();
            (c.visibility, c.module_path.clone())
        };
        if !self.check_visibility(visibility, &declaring_module, path.origin) {
            self.error(
                id,
                span,
                AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                    module: declaring_module,
                    item: cell.borrow().name.clone(),
                }),
            );
            return None;
        }
        Some((cell, raw_args))
    }

    fn resolve_ambient_iterator_spec_cell(
        &mut self,
        name: &str,
    ) -> Option<Rc<RefCell<ResolvedSpecType>>> {
        let name = Ident(name.to_string());
        let path = Path::from(name.clone());
        if let Ok(access) =
            self.context
                .resolve_absolute_item_path(&mut *self.resolver, &path, &self.module_path)
            && let Ok(Some(cell)) = self.resolver.spec_declaration(&access.absolute)
        {
            return Some(cell);
        }
        let ambient = self
            .resolver
            .ambient_core_candidates(&self.module_path, &name)
            .ok()
            .flatten()?;
        self.resolver.spec_declaration(&ambient).ok().flatten()
    }

    pub(super) fn for_in_source_declares(&mut self, ty: &ResolvedType, name: &str) -> bool {
        !self.for_in_conformances(ty, name).is_empty()
    }

    pub(super) fn for_in_conformances(
        &mut self,
        ty: &ResolvedType,
        name: &str,
    ) -> Vec<crate::resolved_type::ResolvedConformance> {
        let Some(target_cell) = self.resolve_ambient_iterator_spec_cell(name) else {
            return vec![];
        };
        match self.resolver.conformances_for_type(ty) {
            Ok(conformances) => conformances
                .into_iter()
                .filter(|conform| conform.spec.borrow().id == target_cell.borrow().id)
                .collect(),
            Err(_) => vec![],
        }
    }

    fn resolve_raw_spec_fn_type(
        &mut self,
        id: HirId,
        span: Span,
        raw: &RawSpecFunctionSig,
        substitution: &GenericSubstitution,
        module: &[Ident],
    ) -> Option<(
        ResolvedFunctionType,
        Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>)>,
    )> {
        self.with_substitution(substitution, |this| {
            let mut params = Vec::with_capacity(raw.params.len());
            let mut ok = true;
            for p in &raw.params {
                match this.resolve_type_or_error_in(id, span, &p.r#type, true, module) {
                    Some(r) => params.push(ResolvedFunctionParam::described(p.ident.clone(), r)),
                    None => ok = false,
                }
            }
            let mut return_type_bound = Vec::new();
            let return_type = match &raw.return_type {
                Type::SpecStatic(members) => {
                    let mut bound_ok = true;
                    for member in members {
                        match this.resolve_spec_application_cell(id, span, member, true, module) {
                            Some((cell, raw_args)) => {
                                let params = cell.borrow().generics.clone();
                                let resolved_args: Option<Vec<ResolvedGenericArg>> = raw_args
                                    .iter()
                                    .enumerate()
                                    .map(|(index, a)| {
                                        this.resolve_generic_arg_in(
                                            id,
                                            span,
                                            a,
                                            params.get(index),
                                            module,
                                        )
                                    })
                                    .collect();
                                match resolved_args {
                                    Some(args) => return_type_bound.push((cell, args)),
                                    None => bound_ok = false,
                                }
                            }
                            None => bound_ok = false,
                        }
                    }
                    if bound_ok {
                        Some(ResolvedType::Void)
                    } else {
                        None
                    }
                }
                other => this.resolve_return_type_or_error_in(id, span, other, true, module),
            };
            if !ok {
                return None;
            }
            Some((
                ResolvedFunctionType {
                    params,
                    return_type: Box::new(return_type?),
                    is_variadic: raw.is_variadic,
                    self_mode: raw.self_mode,
                    calling_convention: crate::resolved_type::CallingConvention::Omega,
                },
                return_type_bound,
            ))
        })
    }

    pub(super) fn flatten_spec(
        &mut self,
        id: HirId,
        span: Span,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedGenericArg],
        self_type: &ResolvedType,
    ) -> Option<Vec<FlattenedSpecFn>> {
        let mut out = Vec::new();
        self.flatten_spec_into(id, span, spec, spec_args, self_type, &mut out)?;
        Some(out)
    }

    fn flatten_spec_into(
        &mut self,
        id: HirId,
        span: Span,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedGenericArg],
        self_type: &ResolvedType,
        out: &mut Vec<FlattenedSpecFn>,
    ) -> Option<()> {
        let (spec_id, spec_name, spec_module, generics, functions) = {
            let s = spec.borrow();
            (
                s.id,
                s.name.clone(),
                s.module_path.clone(),
                s.generics.clone(),
                s.functions.clone(),
            )
        };

        let self_ident = Ident("Self".to_string());
        let substitution: GenericSubstitution =
            std::iter::once((self_ident, ResolvedGenericArg::Type(self_type.clone())))
                .chain(
                    generics
                        .iter()
                        .map(|p| p.ident.clone())
                        .zip(spec_args.iter().cloned()),
                )
                .collect();

        for (name, raw) in &functions {
            let (fn_type, return_type_bound) =
                self.resolve_raw_spec_fn_type(id, span, raw, &substitution, &spec_module)?;
            // Identity dedup only -- same spec, same type args, same name.
            if out.iter().any(|existing| {
                existing.spec_id == spec_id
                    && existing.spec_args() == *spec_args
                    && existing.name == *name
            }) {
                continue;
            }
            out.push(FlattenedSpecFn {
                name: name.clone(),
                fn_type,
                return_type_bound,
                raw: raw.clone(),
                spec_id,
                spec_name: spec_name.clone(),
                visibility: raw.visibility,
                substitution: substitution.clone(),
            });
        }
        Some(())
    }

    fn fn_satisfies_requirement(
        &mut self,
        id: HirId,
        span: Span,
        own: &ResolvedFunctionType,
        req_fn_type: &ResolvedFunctionType,
        req_bound: &[(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>)],
    ) -> bool {
        if req_bound.is_empty() {
            return own == req_fn_type;
        }
        own.self_mode == req_fn_type.self_mode
            && own.is_variadic == req_fn_type.is_variadic
            && own.params == req_fn_type.params
            && req_bound.iter().all(|(spec, spec_args)| {
                self.type_implements_spec(id, span, &own.return_type, spec, spec_args, false)
                    .is_ok()
            })
    }

    /// Bound-set expansion no longer follows spec-alias dependencies (they
    /// were removed with the alias mechanism): a generic parameter's bound
    /// list is already the explicit set of specs it must satisfy.
    pub fn expand_bound_set(
        &mut self,
        _id: HirId,
        _span: Span,
        bounds: &[ResolvedBound],
    ) -> Vec<(HirId, Vec<ResolvedGenericArg>)> {
        let mut out: Vec<(HirId, Vec<ResolvedGenericArg>)> = Vec::new();
        for bound in bounds {
            let key = (bound.spec.borrow().id, bound.spec_args.clone());
            if !out.contains(&key) {
                out.push(key);
            }
        }
        out
    }

    pub(super) fn type_implements_spec(
        &mut self,
        id: HirId,
        span: Span,
        ty: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedGenericArg],
        _check_method_visibility: bool,
    ) -> Result<Vec<HirId>, Vec<Ident>> {
        match self.resolver.conformance_for(ty, spec, spec_args) {
            Ok(Some(conform)) => Ok(conform
                .methods
                .iter()
                .map(|(_, method)| method.decl_id)
                .collect()),
            Ok(None) => {
                let Some(requirements) = self.flatten_spec(id, span, spec, spec_args, ty) else {
                    return Err(vec![]);
                };
                let spec_id = spec.borrow().id;
                let member_ids: Vec<HirId> = vec![spec_id];
                let candidates = match self.resolver.conformances_for_specs(ty, &member_ids) {
                    Ok(entries) => entries,
                    Err(error) => {
                        self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                        return Err(vec![]);
                    }
                };
                let available: Vec<(HirId, Vec<ResolvedGenericArg>, Ident, ResolvedMethod)> =
                    candidates
                        .into_iter()
                        .filter(|entry| entry.spec.borrow().id == spec_id)
                        .flat_map(|entry| {
                            let spec_id = entry.spec.borrow().id;
                            let spec_args = entry.spec_args.clone();
                            entry.methods.into_iter().map(move |(name, method)| {
                                (spec_id, spec_args.clone(), name, method)
                            })
                        })
                        .collect();

                let mut slots = Vec::with_capacity(requirements.len());
                let mut missing = Vec::new();
                for requirement in &requirements {
                    let found = available
                        .iter()
                        .position(|(spec_id, spec_args, name, method)| {
                            *spec_id == requirement.spec_id
                                && *spec_args == requirement.spec_args()
                                && *name == requirement.name
                                && self.fn_satisfies_requirement(
                                    id,
                                    span,
                                    &method.fn_type,
                                    &requirement.fn_type,
                                    &requirement.return_type_bound,
                                )
                        });
                    match found {
                        Some(index) => slots.push(available[index].3.decl_id),
                        None => missing.push(requirement.name.clone()),
                    }
                }
                if missing.is_empty() {
                    Ok(slots)
                } else {
                    Err(missing)
                }
            }
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                Err(vec![])
            }
        }
    }

    /// Conformance of a concrete type to a whole canonical dynamic shape:
    /// every member must be satisfied independently, and its slots
    /// concatenate in canonical shape order. This is the section layout the
    /// vtable follows, so callers that need a single member's section start
    /// offset can sum the slot counts of the members before it.
    pub(super) fn type_implements_shape(
        &mut self,
        id: HirId,
        span: Span,
        ty: &ResolvedType,
        shape: &crate::resolved_type::ResolvedSpecShape,
    ) -> Result<Vec<HirId>, Vec<Ident>> {
        let mut slots = Vec::new();
        let mut missing = Vec::new();
        for member in &shape.members {
            match self.type_implements_spec(id, span, ty, &member.spec, &member.spec_args, false) {
                Ok(member_slots) => slots.extend(member_slots),
                Err(names) => missing.extend(names),
            }
        }
        if missing.is_empty() {
            Ok(slots)
        } else {
            Err(missing)
        }
    }

    /// The compile-time section start offset of one already-present shape
    /// member, purely from each member's own declared requirement count --
    /// section sizes never depend on a concrete conforming type, only on the
    /// shape itself (an object-safe spec may legally have zero methods).
    /// Returns `None` if `target_spec_id` is not a member of `shape`.
    pub(super) fn shape_member_section(
        &mut self,
        id: HirId,
        span: Span,
        shape: &crate::resolved_type::ResolvedSpecShape,
        target_spec_id: HirId,
    ) -> Option<(usize, usize)> {
        let mut offset = 0usize;
        for member in &shape.members {
            let count = self
                .flatten_spec(
                    id,
                    span,
                    &member.spec,
                    &member.spec_args,
                    &ResolvedType::Void,
                )
                .map(|fns| fns.len())
                .unwrap_or(0);
            if member.spec.borrow().id == target_spec_id {
                return Some((offset, count));
            }
            offset += count;
        }
        None
    }

    pub fn check_generic_bound(
        &mut self,
        id: HirId,
        span: Span,
        bound: &Type,
        concrete: &ResolvedType,
    ) -> Option<Result<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedGenericArg>), (Ident, Vec<Ident>)>>
    {
        let (spec, spec_args) = self.resolve_spec_reference(id, span, bound)?;
        let spec_name = spec.borrow().name.clone();
        match self.type_implements_spec(id, span, concrete, &spec, &spec_args, false) {
            Ok(_) => Some(Ok((spec, spec_args))),
            Err(missing) => Some(Err((spec_name, missing))),
        }
    }
}
