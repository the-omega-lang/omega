use super::*;
use crate::generics::GenericParam;
use crate::generics::pattern::{ArgumentPattern, TypePattern};

/// One positional generic inference problem: a declaration's parameters,
/// what is bound so far, and what else the declaration's own syntax may
/// name. Every inferring call site -- a generic function, a generic member of
/// a concrete or generic owner, a generic aggregate literal -- solves one of
/// these against the declaration's pattern signature.
pub(crate) struct Inference<'t> {
    pub(crate) generics: &'t [GenericParam],
    pub(crate) comp_types: &'t [Option<TypePattern>],
    /// What an enclosing owner instantiation binds: a default evaluated for
    /// a hint may name an owner parameter or `Self`.
    pub(crate) enclosing: &'t GenericSubstitution,
    pub(crate) bindings: Vec<Option<ResolvedGenericArg>>,
}

impl<'t> Inference<'t> {
    pub(crate) fn new(
        generics: &'t [GenericParam],
        comp_types: &'t [Option<TypePattern>],
        enclosing: &'t GenericSubstitution,
        mut seed: Vec<Option<ResolvedGenericArg>>,
    ) -> Self {
        seed.resize(generics.len(), None);
        Self {
            generics,
            comp_types,
            enclosing,
            bindings: seed,
        }
    }

    pub(crate) fn of_template(
        template: &'t OverloadTemplate,
        enclosing: &'t GenericSubstitution,
        seed: Vec<Option<ResolvedGenericArg>>,
    ) -> Self {
        Self::new(&template.generics, &template.comp_types, enclosing, seed)
    }

    /// Seeds what the expected result type determines. Positions already
    /// bound -- explicitly written arguments -- outrank the expectation: the
    /// caller asked for them, so a conflict is the ordinary result check's to
    /// report rather than inference's to reinterpret.
    pub(crate) fn seed_from_expected(&mut self, return_type: &TypePattern, expected: Expected<'_>) {
        match expected {
            Expected::None => {}
            Expected::Exact(expected) => return_type.infer(expected, &mut self.bindings),
            Expected::Pattern(expected) => {
                return_type.infer_from_pattern(expected, &mut self.bindings)
            }
        }
    }

    /// Everything bound so far, for evaluating the declaration's own syntax.
    fn substitution(&self) -> GenericSubstitution {
        self.enclosing
            .enclosing(&GenericSubstitution::from_bindings(
                self.generics,
                &self.bindings,
            ))
    }

    /// Each bound `comp` position rewritten into its declared type, which
    /// is authoritative. A value the declared type cannot represent leaves
    /// its position unbound, so it is reported as undetermined.
    pub(crate) fn canonicalize_comp_bindings(&mut self, pointer_bits: u32) {
        for position in 0..self.bindings.len() {
            let Some(ResolvedGenericArg::Comp(value)) = self.bindings[position] else {
                continue;
            };
            let kind = self
                .comp_types
                .get(position)
                .and_then(Option::as_ref)
                .and_then(|pattern| pattern.resolved(&self.bindings))
                .and_then(|ty| CompScalarType::from_resolved(&ty));
            self.bindings[position] = kind
                .and_then(|kind| CompScalar::normalize(&value.const_value(), kind, pointer_bits))
                .map(ResolvedGenericArg::Comp);
        }
    }

    /// The parameter written as a bare pointer whose argument was a fat
    /// pointer: `*T` cannot bind `T` from a slice or `*str`, and saying so is
    /// clearer than reporting `T` as merely undetermined.
    pub(crate) fn fat_pointer_mismatch(
        &self,
        params: &[TypePattern],
        args: &[CheckedExprNode],
    ) -> Option<(String, ResolvedType)> {
        params.iter().zip(args).find_map(|(pattern, arg)| {
            let TypePattern::Pointer(inner, _) = pattern else {
                return None;
            };
            let TypePattern::Parameter(position) = inner.as_ref() else {
                return None;
            };
            (self.bindings[*position].is_none()
                && matches!(
                    arg.r#type,
                    ResolvedType::Slice { .. } | ResolvedType::Str { .. }
                ))
            .then(|| (self.generics[*position].label(), arg.r#type.clone()))
        })
    }
}

impl<'r> Analyzer<'r> {
    /// Analyzes each argument against its parameter pattern, binding the
    /// positions each checked type determines. A parameter whose type is
    /// already determined -- by what is bound, or by the declaration's
    /// defaults -- steers its argument like any other expected type.
    pub(crate) fn infer_arguments(
        &mut self,
        inference: &mut Inference<'_>,
        params: &[TypePattern],
        args: &[HirExprNode],
    ) -> Option<Vec<CheckedExprNode>> {
        let mut checked_args = Vec::with_capacity(args.len());
        for (pattern, arg) in params.iter().zip(args) {
            let expected = self.pattern_hint(arg.id, arg.span, inference, pattern);
            let checked = self.analyze_expr(arg, expected.as_ref().into())?;
            pattern.infer(&checked.r#type, &mut inference.bindings);
            checked_args.push(checked);
        }
        inference.canonicalize_comp_bindings(self.target.pointer_bits());
        Some(checked_args)
    }

    /// The argument list inference produced, completed from the
    /// declaration's defaults by whoever owns `target`. `None` when a
    /// position is undetermined, which `report` describes.
    pub(crate) fn solved_generic_args(
        inference: &Inference<'_>,
    ) -> Result<Vec<Option<ResolvedGenericArg>>, usize> {
        crate::generics::resolve_inferred_generic_args(inference.generics, &inference.bindings)
    }

    /// The expected type one parameter places on its argument under what is
    /// bound so far, with the declaration's defaults standing in for the
    /// positions still open.
    ///
    /// This is a hint, so nothing here is reported: a declaration whose
    /// types or defaults do not resolve is diagnosed where its signature is
    /// analyzed, not again at every call that tries to read a hint from it.
    pub(crate) fn pattern_hint(
        &mut self,
        id: HirId,
        span: Span,
        inference: &Inference<'_>,
        pattern: &TypePattern,
    ) -> Option<ResolvedType> {
        self.without_diagnostics(|this| {
            let mut bindings = inference.bindings.clone();
            if bindings.iter().any(Option::is_none) {
                let mut local = inference.substitution();
                for (position, param) in inference.generics.iter().enumerate() {
                    if bindings[position].is_some() {
                        continue;
                    }
                    let Some(default) = &param.default else {
                        continue;
                    };
                    let Some(resolved) =
                        this.resolve_default_generic_arg(id, span, param, default, &local)
                    else {
                        continue;
                    };
                    local.bind(param, resolved.clone());
                    bindings[position] = Some(resolved);
                }
            }
            this.pattern_type(pattern, &bindings)
        })
    }

    /// The concrete type `pattern` denotes under `bindings`, materializing
    /// any nominal or spec-object application it contains. `None` while a
    /// position it mentions is unbound.
    pub(crate) fn pattern_type(
        &mut self,
        pattern: &TypePattern,
        bindings: &[Option<ResolvedGenericArg>],
    ) -> Option<ResolvedType> {
        Some(match pattern {
            TypePattern::Fixed(_) | TypePattern::Parameter(_) => pattern.resolved(bindings)?,
            TypePattern::Pointer(inner, mutable) => ResolvedType::Pointer {
                pointee: Box::new(self.pattern_type(inner, bindings)?),
                mutable: *mutable,
            },
            TypePattern::Slice(inner, mutable) => ResolvedType::Slice {
                item: Box::new(self.pattern_type(inner, bindings)?),
                mutable: *mutable,
            },
            TypePattern::Array(inner, mutable) => {
                ResolvedType::Array(Box::new(self.pattern_type(inner, bindings)?), *mutable)
            }
            TypePattern::SizedArray(inner, length) => ResolvedType::SizedArray(
                Box::new(self.pattern_type(inner, bindings)?),
                length.length(bindings)?,
            ),
            TypePattern::Function(params, result, convention, variadic) => {
                let mut resolved = Vec::with_capacity(params.len());
                for param in params {
                    resolved.push(crate::resolved_type::ResolvedFunctionParam::anonymous(
                        self.pattern_type(param, bindings)?,
                    ));
                }
                ResolvedType::Function(ResolvedFunctionType {
                    params: resolved,
                    return_type: Box::new(self.pattern_type(result, bindings)?),
                    calling_convention: *convention,
                    is_variadic: *variadic,
                    self_mode: None,
                })
            }
            TypePattern::AnonymousEnum(members) => {
                let mut resolved = Vec::with_capacity(members.len());
                for member in members {
                    resolved.push(self.pattern_type(member, bindings)?);
                }
                ResolvedType::AnonymousEnum {
                    shape: Rc::new(ResolvedAnonymousEnum::canonicalize(resolved)),
                    variant: None,
                }
            }
            TypePattern::Nominal(path, args) => {
                let args = self.pattern_args(args, bindings)?;
                // The declaration that wrote this type was already allowed to
                // name it; the caller only borrows that type for a hint.
                match self.resolver.resolve_item(
                    &self.module_path,
                    path,
                    &args,
                    ResolveItemOptions::INDIRECT.bypassing_visibility(true),
                ) {
                    Ok(ResolvedItem::Type(r#type)) => r#type,
                    _ => return None,
                }
            }
            TypePattern::SpecObject(members, mutable) => {
                let mut applications = Vec::with_capacity(members.len());
                for member in members {
                    let spec = self
                        .resolver
                        .spec_declaration(&member.module_path.join(member.name.clone()))
                        .ok()??;
                    let args = self.pattern_args(&member.args, bindings)?;
                    applications.push(crate::resolved_type::ResolvedSpecApplication::new(
                        spec, args,
                    ));
                }
                ResolvedType::SpecObject {
                    shape: crate::resolved_type::ResolvedSpecShape::canonicalize(applications),
                    mutable: *mutable,
                }
            }
        })
    }

    fn pattern_args(
        &mut self,
        args: &[ArgumentPattern],
        bindings: &[Option<ResolvedGenericArg>],
    ) -> Option<Vec<ResolvedGenericArg>> {
        args.iter()
            .map(|arg| match arg {
                ArgumentPattern::Type(pattern) => self
                    .pattern_type(pattern, bindings)
                    .map(ResolvedGenericArg::Type),
                ArgumentPattern::Value(value) => Some(ResolvedGenericArg::Comp(*value)),
                ArgumentPattern::Parameter(position, kind) => match bindings.get(*position)? {
                    Some(ResolvedGenericArg::Comp(value)) => CompScalar::normalize(
                        &value.const_value(),
                        *kind,
                        self.target.pointer_bits(),
                    )
                    .map(ResolvedGenericArg::Comp),
                    _ => None,
                },
            })
            .collect()
    }
}
