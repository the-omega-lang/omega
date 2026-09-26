use super::*;
use crate::generics::pattern::{ArgumentPattern, SpecPattern, TypePattern};

impl Analyzer<'_> {
    pub fn overload_template(&mut self, function: &HirFunctionDef) -> Option<OverloadTemplate> {
        let function = self.normalized_function(function)?;
        let generics = &function.generics;
        let mut bounds = Vec::new();
        for (parameter, generic) in generics.iter().enumerate() {
            let expanded = match crate::aliases::expand_bounds(
                self.resolver,
                &self.module_path,
                generic.bounds(),
            ) {
                Ok(bounds) => bounds,
                Err(error) => {
                    self.error(
                        function.id,
                        function.span,
                        AnalysisErrorKind::ModuleResolution(error),
                    );
                    return None;
                }
            };
            for bound in expanded {
                // `expand_bounds` flattens conjunctions but hands back what
                // was written for anything else, so that alias obligations
                // stay checkable at their own spelling. A pattern needs the
                // spec the bound actually names.
                let bound = match crate::aliases::expand_type_alias(
                    self.resolver,
                    &self.module_path,
                    bound,
                ) {
                    Ok(bound) => bound,
                    Err(error) => {
                        self.error(
                            function.id,
                            function.span,
                            AnalysisErrorKind::ModuleResolution(error),
                        );
                        return None;
                    }
                };
                let (path, args) = match &bound {
                    Type::Named(path) => (path, &[][..]),
                    Type::Generic(path, args) => (path, args.as_slice()),
                    _ => return None,
                };
                let absolute = self.pattern_item_path(function.id, function.span, path)?;
                let spec = match self.resolver.spec_declaration(&absolute) {
                    Ok(Some(spec)) => spec,
                    Ok(None) => {
                        self.error(
                            function.id,
                            function.span,
                            AnalysisErrorKind::UnresolvedType(TypeResolutionError::NotASpec(
                                path.head.clone(),
                            )),
                        );
                        return None;
                    }
                    Err(error) => {
                        self.error(
                            function.id,
                            function.span,
                            AnalysisErrorKind::ModuleResolution(error),
                        );
                        return None;
                    }
                };
                let spec = spec.borrow();
                let args = self.argument_patterns(
                    function.id,
                    function.span,
                    args,
                    generics,
                    &spec.generics,
                    &spec.module_path,
                    PatternPurpose::Overload,
                )?;
                let key = (
                    parameter,
                    SpecPattern {
                        spec: spec.id,
                        module_path: spec.module_path.clone(),
                        name: spec.name.clone(),
                        args,
                    },
                );
                if !bounds.contains(&key) {
                    bounds.push(key);
                }
            }
        }
        let params = function
            .params
            .iter()
            .map(|param| {
                self.overload_type_pattern(function.id, function.span, &param.r#type, generics)
            })
            .collect::<Option<Vec<_>>>()?;
        let return_type = self.overload_type_pattern(
            function.id,
            function.span,
            &function.return_type,
            generics,
        )?;
        let comp_types = generics
            .iter()
            .map(|generic| {
                let raw = generic.comp_type()?;
                self.overload_type_pattern(function.id, function.span, raw, generics)
            })
            .collect();
        Some(OverloadTemplate {
            generics: generics.clone(),
            params,
            return_type,
            comp_types,
            bounds,
            // A function *definition* has no convention or variadic syntax of
            // its own; see `collect_function_signature`, which gives every
            // instantiation of this template the same pair.
            calling_convention: CallingConvention::Omega,
            is_variadic: false,
            description: format!(
                "<{}>({}) => {}",
                generics
                    .iter()
                    .map(|p| {
                        if p.bounds().is_empty() {
                            p.ident.to_string()
                        } else {
                            format!(
                                "{}: {}",
                                p.ident,
                                p.bounds()
                                    .iter()
                                    .map(crate::error::raw_type_display)
                                    .collect::<Vec<_>>()
                                    .join(" + ")
                            )
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                function
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.ident, crate::error::raw_type_display(&p.r#type)))
                    .collect::<Vec<_>>()
                    .join(", "),
                crate::error::raw_type_display(&function.return_type)
            ),
        })
    }

    /// The identity of a generic function *declaration*, taken in the
    /// context it is declared in and before its own generic parameters are
    /// substituted. `None` for a declaration with no generic parameters of
    /// its own, whose ordinary symbol already identifies it.
    pub fn template_descriptor(
        &mut self,
        function: &HirFunctionDef,
    ) -> Option<crate::template::TemplateDescriptor> {
        let template = self.overload_template(function)?;
        if template.generics.is_empty() {
            return None;
        }
        crate::template::TemplateDescriptor::of(&template)
    }

    fn argument_patterns(
        &mut self,
        id: HirId,
        span: Span,
        written: &[GenericArg],
        generics: &[HirGenericParam],
        declared: &[HirGenericParam],
        module: &[Ident],
        purpose: PatternPurpose<'_>,
    ) -> Option<Vec<ArgumentPattern>> {
        let mut args = written
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                self.argument_pattern(
                    id,
                    span,
                    arg,
                    generics,
                    declared.get(index),
                    module,
                    purpose,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        for index in args.len()..declared.len() {
            let parameter = &declared[index];
            let Some(default) = &parameter.default else {
                break;
            };
            // An annotation's default sees the arguments before it: those
            // already known are substituted by name, so only the others stay
            // open in its pattern.
            let (known, open) = match purpose {
                PatternPurpose::Overload => (GenericSubstitution::new(), Vec::new()),
                PatternPurpose::Annotation { .. } => {
                    let mut known = GenericSubstitution::new();
                    let mut open = Vec::with_capacity(index);
                    for (param, arg) in declared.iter().zip(&args) {
                        match arg.known() {
                            Some(arg) => {
                                known.push(param.ident.clone(), arg);
                                open.push(false);
                            }
                            None => open.push(true),
                        }
                    }
                    (known, open)
                }
            };
            let default_purpose = match purpose {
                PatternPurpose::Overload => PatternPurpose::Overload,
                PatternPurpose::Annotation { .. } => PatternPurpose::Annotation { open: &open },
            };
            // Resolve before substitution: a default's names belong to its
            // declaration, even if the function has a parameter of that name.
            let saved = std::mem::replace(&mut self.module_path, module.to_vec());
            let saved_context = std::mem::replace(&mut self.context, Context::new(self.target));
            let pattern = self.with_substitution(&known, |this| {
                this.argument_pattern(
                    id,
                    span,
                    default,
                    &declared[..index],
                    Some(parameter),
                    module,
                    default_purpose,
                )
            });
            self.context = saved_context;
            self.module_path = saved;
            args.push(pattern?.substitute(&args, self.target.pointer_bits())?);
        }
        Some(args)
    }

    pub(crate) fn pattern_item_path(
        &mut self,
        id: HirId,
        span: Span,
        path: &Path,
    ) -> Option<Vec<Ident>> {
        let access =
            match self
                .context
                .resolve_absolute_item_path(self.resolver, path, &self.module_path)
            {
                Ok(access) => access,
                Err(error) => {
                    self.error(id, span, AnalysisErrorKind::UnresolvedType(error));
                    return None;
                }
            };
        match self.resolver.item_generic_params(&access.absolute) {
            Ok(Some(_)) => Some(access.absolute),
            result => {
                if path.is_unqualified()
                    && let Ok(Some(ambient)) = self
                        .resolver
                        .ambient_core_candidates(&self.module_path, &path.head)
                {
                    return Some(ambient);
                }
                if let Err(error) = result {
                    self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                    return None;
                }
                Some(access.absolute)
            }
        }
    }

    pub(crate) fn overload_type_pattern(
        &mut self,
        id: HirId,
        span: Span,
        raw: &Type,
        generics: &[HirGenericParam],
    ) -> Option<TypePattern> {
        self.type_pattern(id, span, raw, generics, PatternPurpose::Overload)
    }

    /// The pattern of a binding annotation whose holes were rewritten to
    /// `holes`. Every part that names no hole is resolved as written.
    pub(crate) fn annotation_type_pattern(
        &mut self,
        id: HirId,
        span: Span,
        raw: &Type,
        holes: &[HirGenericParam],
    ) -> Option<TypePattern> {
        let open = vec![true; holes.len()];
        self.type_pattern(
            id,
            span,
            raw,
            holes,
            PatternPurpose::Annotation { open: &open },
        )
    }

    fn type_pattern(
        &mut self,
        id: HirId,
        span: Span,
        raw: &Type,
        generics: &[HirGenericParam],
        purpose: PatternPurpose<'_>,
    ) -> Option<TypePattern> {
        if let Type::Named(path) = raw
            && let Some(index) = purpose.parameter(generics, path, false)
        {
            return Some(TypePattern::Parameter(index));
        }
        let raw = match crate::aliases::expand_template_type(
            self.resolver,
            &self.module_path,
            &generics.iter().map(|g| g.ident.clone()).collect::<Vec<_>>(),
            raw,
        ) {
            Ok(raw) => raw,
            Err(error) => {
                self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                return None;
            }
        };
        if purpose.reports() && !purpose.mentions_open(&raw, generics) {
            return Some(TypePattern::Fixed(
                self.resolve_type_or_error(id, span, &raw, true)?,
            ));
        }
        Some(match &raw {
            Type::Named(path) if purpose.parameter(generics, path, false).is_some() => {
                TypePattern::Parameter(purpose.parameter(generics, path, false).unwrap())
            }
            Type::Named(_) => TypePattern::Fixed(self.resolve_type_or_error(id, span, &raw, true)?),
            Type::Pointer(inner, mutable) => match inner.as_ref() {
                Type::InferredArray(item) => TypePattern::Slice(
                    Box::new(self.type_pattern(id, span, item, generics, purpose)?),
                    *mutable,
                ),
                Type::UnknownSizeArray(item) => TypePattern::Array(
                    Box::new(self.type_pattern(id, span, item, generics, purpose)?),
                    *mutable,
                ),
                Type::SpecStatic(members) => {
                    let mut patterns = Vec::new();
                    for member in members {
                        let (path, args) = match member {
                            Type::Named(path) => (path, &[][..]),
                            Type::Generic(path, args) => (path, args.as_slice()),
                            _ => {
                                if purpose.reports() {
                                    self.error(
                                        id,
                                        span,
                                        AnalysisErrorKind::UnresolvedType(
                                            TypeResolutionError::NotASpec(Ident(
                                                "<spec>".to_string(),
                                            )),
                                        ),
                                    );
                                }
                                return None;
                            }
                        };
                        let absolute = self.pattern_item_path(id, span, path)?;
                        let spec = match self.resolver.spec_declaration(&absolute) {
                            Ok(Some(spec)) => spec,
                            _ => {
                                self.error(
                                    id,
                                    span,
                                    AnalysisErrorKind::UnresolvedType(
                                        TypeResolutionError::NotASpec(path.head.clone()),
                                    ),
                                );
                                return None;
                            }
                        };
                        let spec = spec.borrow();
                        let args = self.argument_patterns(
                            id,
                            span,
                            args,
                            generics,
                            &spec.generics,
                            &spec.module_path,
                            purpose,
                        )?;
                        let key = SpecPattern {
                            spec: spec.id,
                            module_path: spec.module_path.clone(),
                            name: spec.name.clone(),
                            args,
                        };
                        if !patterns.contains(&key) {
                            patterns.push(key);
                        }
                    }
                    TypePattern::SpecObject(patterns, *mutable)
                }
                Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "str" => {
                    TypePattern::Fixed(ResolvedType::Str { mutable: *mutable })
                }
                _ => TypePattern::Pointer(
                    Box::new(self.type_pattern(id, span, inner, generics, purpose)?),
                    *mutable,
                ),
            },
            Type::SizedArray(inner, length) => {
                let length = match length {
                    ArrayLength::Path(path)
                        if purpose.parameter(generics, path, true).is_some() =>
                    {
                        ArgumentPattern::Parameter(
                            purpose.parameter(generics, path, true).unwrap(),
                            CompScalarType::Int(crate::resolved_type::CompIntType::USize),
                        )
                    }
                    _ => {
                        let ty = Type::SizedArray(
                            Box::new(Type::Named(Ident("u8".into()).into())),
                            length.clone(),
                        );
                        let ResolvedType::SizedArray(_, size) =
                            self.resolve_type_or_error(id, span, &ty, true)?
                        else {
                            return None;
                        };
                        ArgumentPattern::Value(CompScalar::Int {
                            r#type: crate::resolved_type::CompIntType::USize,
                            value: i128::from(size),
                        })
                    }
                };
                TypePattern::SizedArray(
                    Box::new(self.type_pattern(id, span, inner, generics, purpose)?),
                    length,
                )
            }
            Type::Generic(path, args) => {
                let absolute = self.pattern_item_path(id, span, path)?;
                let declared = match self.resolver.item_generic_params(&absolute) {
                    Ok(Some(params)) => params,
                    Ok(None) => Vec::new(),
                    Err(error) => {
                        self.error(id, span, AnalysisErrorKind::ModuleResolution(error));
                        return None;
                    }
                };
                let (module, item) = absolute.split_at(absolute.len() - 1);
                if purpose.reports() && !declared.is_empty() && args.len() > declared.len() {
                    self.error(
                        id,
                        span,
                        AnalysisErrorKind::ModuleResolution(
                            ResolveError::GenericArgCountMismatch {
                                module: module.to_vec(),
                                item: item[0].clone(),
                                expected: declared.len(),
                                found: args.len(),
                            },
                        ),
                    );
                    return None;
                }
                let args =
                    self.argument_patterns(id, span, args, generics, &declared, module, purpose)?;
                TypePattern::Nominal(absolute, args)
            }
            Type::Function(function) => {
                let convention = match self
                    .context
                    .resolve_convention(function.convention.as_ref().map(|c| &c.name))
                {
                    Ok(convention) => convention,
                    Err(error) => {
                        if purpose.reports() {
                            self.error(id, span, AnalysisErrorKind::UnresolvedType(error));
                        }
                        return None;
                    }
                };
                TypePattern::Function(
                    function
                        .params
                        .iter()
                        .map(|p| self.type_pattern(id, span, &p.r#type, generics, purpose))
                        .collect::<Option<_>>()?,
                    Box::new(self.type_pattern(
                        id,
                        span,
                        &function.return_type,
                        generics,
                        purpose,
                    )?),
                    convention,
                    function.is_variadic,
                )
            }
            Type::AnonymousEnum(members) => TypePattern::AnonymousEnum(
                members
                    .iter()
                    .map(|p| self.type_pattern(id, span, p, generics, purpose))
                    .collect::<Option<_>>()?,
            ),
            _ => TypePattern::Fixed(self.resolve_type_or_error(id, span, &raw, true)?),
        })
    }

    fn argument_pattern(
        &mut self,
        id: HirId,
        span: Span,
        arg: &GenericArg,
        generics: &[HirGenericParam],
        declared: Option<&HirGenericParam>,
        module: &[Ident],
        purpose: PatternPurpose<'_>,
    ) -> Option<ArgumentPattern> {
        if declared.is_some_and(|p| p.is_comp()) {
            if let GenericArg::Type(Type::Named(path)) = arg
                && let Some(index) = purpose.parameter(generics, path, true)
            {
                let saved_context = std::mem::replace(&mut self.context, Context::new(self.target));
                let value_type =
                    self.resolve_type_or_error_in(id, span, declared?.comp_type()?, true, module);
                self.context = saved_context;
                let kind = CompScalarType::from_resolved(&value_type?)?;
                return Some(ArgumentPattern::Parameter(index, kind));
            }
            return match self.resolve_generic_arg_or_error(id, span, arg, declared)? {
                ResolvedGenericArg::Comp(value) => Some(ArgumentPattern::Value(value)),
                _ => None,
            };
        }
        let GenericArg::Type(ty) = arg else {
            if purpose.reports() {
                // Always an error: a value or `_` where a type is expected.
                self.resolve_generic_arg_or_error(id, span, arg, declared);
            }
            return None;
        };
        Some(ArgumentPattern::Type(Box::new(
            self.type_pattern(id, span, ty, generics, purpose)?,
        )))
    }
}

/// What a pattern is built for.
#[derive(Clone, Copy)]
enum PatternPurpose<'a> {
    /// An overload declaration: every generic parameter stays symbolic, and
    /// the shape is also the template's identity.
    Overload,
    /// A binding annotation, where only the `open` parameters are unknown.
    /// Every other part is resolved as written, so it constrains inference
    /// exactly, and every failure is reported.
    Annotation { open: &'a [bool] },
}

impl PatternPurpose<'_> {
    fn reports(self) -> bool {
        matches!(self, Self::Annotation { .. })
    }

    fn is_open(self, index: usize) -> bool {
        match self {
            Self::Overload => true,
            Self::Annotation { open } => open[index],
        }
    }

    /// The open parameter of kind `comp` that `path` names.
    fn parameter(self, generics: &[HirGenericParam], path: &Path, comp: bool) -> Option<usize> {
        if !path.is_unqualified() {
            return None;
        }
        generics
            .iter()
            .position(|p| p.ident == path.head)
            .filter(|&index| generics[index].is_comp() == comp && self.is_open(index))
    }

    fn names_open(self, generics: &[HirGenericParam], path: &Path) -> bool {
        self.parameter(generics, path, false).is_some()
            || self.parameter(generics, path, true).is_some()
    }

    fn mentions_open(self, ty: &Type, generics: &[HirGenericParam]) -> bool {
        match ty {
            Type::Named(path) => self.names_open(generics, path),
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner) => self.mentions_open(inner, generics),
            Type::SizedArray(inner, length) => {
                matches!(length, ArrayLength::Path(path) if self.names_open(generics, path))
                    || self.mentions_open(inner, generics)
            }
            Type::Generic(_, args) => args.iter().any(|arg| {
                arg.as_type()
                    .is_some_and(|ty| self.mentions_open(ty, generics))
            }),
            Type::Function(function) => {
                function
                    .params
                    .iter()
                    .any(|param| self.mentions_open(&param.r#type, generics))
                    || self.mentions_open(&function.return_type, generics)
            }
            Type::SpecStatic(members) | Type::AnonymousEnum(members) => members
                .iter()
                .any(|member| self.mentions_open(member, generics)),
            Type::Infer => false,
        }
    }
}
