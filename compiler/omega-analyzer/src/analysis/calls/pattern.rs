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
                let args = self.overload_argument_patterns(
                    function.id,
                    function.span,
                    args,
                    generics,
                    &spec.generics,
                    &spec.module_path,
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

    fn overload_argument_patterns(
        &mut self,
        id: HirId,
        span: Span,
        written: &[GenericArg],
        generics: &[HirGenericParam],
        declared: &[HirGenericParam],
        module: &[Ident],
    ) -> Option<Vec<ArgumentPattern>> {
        let mut args = written
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                self.overload_argument_pattern(id, span, arg, generics, declared.get(index), module)
            })
            .collect::<Option<Vec<_>>>()?;
        for index in args.len()..declared.len() {
            let parameter = &declared[index];
            let Some(default) = &parameter.default else {
                break;
            };
            // Resolve before substitution: a default's names belong to its
            // declaration, even if the function has a parameter of that name.
            let saved = std::mem::replace(&mut self.module_path, module.to_vec());
            let saved_context = std::mem::replace(&mut self.context, Context::new(self.target));
            let pattern = self.overload_argument_pattern(
                id,
                span,
                default,
                &declared[..index],
                Some(parameter),
                module,
            );
            self.context = saved_context;
            self.module_path = saved;
            args.push(pattern?.substitute(&args, self.target.pointer_bits())?);
        }
        Some(args)
    }

    fn pattern_item_path(&mut self, id: HirId, span: Span, path: &Path) -> Option<Vec<Ident>> {
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

    fn overload_type_pattern(
        &mut self,
        id: HirId,
        span: Span,
        raw: &Type,
        generics: &[HirGenericParam],
    ) -> Option<TypePattern> {
        if let Type::Named(path) = raw
            && path.is_unqualified()
            && let Some(index) = generics
                .iter()
                .position(|p| p.ident == path.head && !p.is_comp())
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
        Some(match &raw {
            Type::Named(path)
                if path.is_unqualified()
                    && generics
                        .iter()
                        .any(|p| p.ident == path.head && !p.is_comp()) =>
            {
                TypePattern::Parameter(generics.iter().position(|p| p.ident == path.head).unwrap())
            }
            Type::Named(_) => TypePattern::Fixed(self.resolve_type_or_error(id, span, &raw, true)?),
            Type::Pointer(inner, mutable) => match inner.as_ref() {
                Type::InferredArray(item) => TypePattern::Slice(
                    Box::new(self.overload_type_pattern(id, span, item, generics)?),
                    *mutable,
                ),
                Type::UnknownSizeArray(item) => TypePattern::Array(
                    Box::new(self.overload_type_pattern(id, span, item, generics)?),
                    *mutable,
                ),
                Type::SpecStatic(members) => {
                    let mut patterns = Vec::new();
                    for member in members {
                        let (path, args) = match member {
                            Type::Named(path) => (path, &[][..]),
                            Type::Generic(path, args) => (path, args.as_slice()),
                            _ => return None,
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
                        let args = self.overload_argument_patterns(
                            id,
                            span,
                            args,
                            generics,
                            &spec.generics,
                            &spec.module_path,
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
                    Box::new(self.overload_type_pattern(id, span, inner, generics)?),
                    *mutable,
                ),
            },
            Type::SizedArray(inner, length) => {
                let length = match length {
                    ArrayLength::Path(path)
                        if path.is_unqualified()
                            && generics.iter().any(|p| p.ident == path.head && p.is_comp()) =>
                    {
                        ArgumentPattern::Parameter(
                            generics.iter().position(|p| p.ident == path.head).unwrap(),
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
                    Box::new(self.overload_type_pattern(id, span, inner, generics)?),
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
                let module = &absolute[..absolute.len() - 1];
                let args =
                    self.overload_argument_patterns(id, span, args, generics, &declared, module)?;
                TypePattern::Nominal(absolute, args)
            }
            Type::Function(function) => {
                let convention = match function.convention.as_ref().map(|c| c.name.as_ref()) {
                    None => CallingConvention::Omega,
                    Some("c") => CallingConvention::C,
                    Some("sysv64") => CallingConvention::SysV64,
                    _ => return None,
                };
                TypePattern::Function(
                    function
                        .params
                        .iter()
                        .map(|p| self.overload_type_pattern(id, span, &p.r#type, generics))
                        .collect::<Option<_>>()?,
                    Box::new(self.overload_type_pattern(
                        id,
                        span,
                        &function.return_type,
                        generics,
                    )?),
                    convention,
                    function.is_variadic,
                )
            }
            Type::AnonymousEnum(members) => TypePattern::AnonymousEnum(
                members
                    .iter()
                    .map(|p| self.overload_type_pattern(id, span, p, generics))
                    .collect::<Option<_>>()?,
            ),
            _ => TypePattern::Fixed(self.resolve_type_or_error(id, span, &raw, true)?),
        })
    }

    fn overload_argument_pattern(
        &mut self,
        id: HirId,
        span: Span,
        arg: &GenericArg,
        generics: &[HirGenericParam],
        declared: Option<&HirGenericParam>,
        module: &[Ident],
    ) -> Option<ArgumentPattern> {
        if declared.is_some_and(|p| p.is_comp()) {
            if let GenericArg::Type(Type::Named(path)) = arg
                && path.is_unqualified()
                && let Some(index) = generics
                    .iter()
                    .position(|p| p.ident == path.head && p.is_comp())
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
            return None;
        };
        Some(ArgumentPattern::Type(Box::new(
            self.overload_type_pattern(id, span, ty, generics)?,
        )))
    }
}
