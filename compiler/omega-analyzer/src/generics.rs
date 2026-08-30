use crate::resolved_type::{CompScalar, CompScalarType, ResolvedGenericArg, ResolvedType};
use crate::resolver::{ModuleResolver, ResolveError};
use omega_hir::{HirFunctionDef, HirGenericParam};
use omega_parser::prelude::{ArrayLength, GenericArg, GenericParamKind, Ident, Type};

/// Rewrites each outermost static `spec ...` parameter into the anonymous
/// bounded generic the rest of the compiler already understands, after
/// expanding aliases so that `f(x: spec A + B)` and `f(x: AB)` normalize
/// identically. Only a parameter's outermost type participates: `*spec A + B`
/// is a dynamic object, and a spec buried in an array, generic argument, or
/// function type is not a parameter bound at all.
///
/// This is the one place the rule lives; every query that reports a function's
/// generics, signature, or body works from the result.
pub fn normalize_static_spec_params(
    resolver: &mut dyn ModuleResolver,
    module_path: &[Ident],
    f: &HirFunctionDef,
) -> Result<HirFunctionDef, ResolveError> {
    let mut normalized: Option<HirFunctionDef> = None;
    for (index, param) in f.params.iter().enumerate() {
        let expanded =
            crate::aliases::expand_type_alias(resolver, module_path, param.r#type.clone())?;
        let Type::SpecStatic(members) = expanded else {
            continue;
        };
        let target = normalized.get_or_insert_with(|| f.clone());
        let fresh = Ident(format!("$Param{index}"));
        target
            .generics
            .push(HirGenericParam::r#type(fresh.clone(), members, None));
        target.params[index].r#type = Type::Named(fresh.into());
    }
    Ok(normalized.unwrap_or_else(|| f.clone()))
}

/// The bindings one generic instantiation installs while the instantiated
/// declaration is analyzed. Entries stay in declaration order, and a name is
/// bound either to a type or to a canonical compile-time value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenericSubstitution {
    entries: Vec<(Ident, ResolvedGenericArg)>,
}

impl GenericSubstitution {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pairs declared parameters with the arguments an instantiation
    /// resolved for them. Surplus on either side is ignored; argument count
    /// is checked where the instantiation is formed.
    pub fn zip<'a>(
        names: impl IntoIterator<Item = &'a Ident>,
        args: &[ResolvedGenericArg],
    ) -> Self {
        Self {
            entries: names
                .into_iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect(),
        }
    }

    pub fn push(&mut self, name: Ident, arg: ResolvedGenericArg) {
        self.entries.push((name, arg));
    }

    pub fn push_type(&mut self, name: Ident, r#type: ResolvedType) {
        self.push(name, ResolvedGenericArg::Type(r#type));
    }

    /// Records `arg` only if `name` is still unbound, so the first binding
    /// an inference pass finds wins.
    pub fn bind_if_absent(&mut self, name: &Ident, arg: impl FnOnce() -> ResolvedGenericArg) {
        if !self.contains(name) {
            self.entries.push((name.clone(), arg()));
        }
    }

    pub fn get(&self, name: &Ident) -> Option<&ResolvedGenericArg> {
        self.entries
            .iter()
            .find_map(|(bound, arg)| (bound == name).then_some(arg))
    }

    pub fn contains(&self, name: &Ident) -> bool {
        self.get(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Ident, &ResolvedGenericArg)> {
        self.entries.iter().map(|(name, arg)| (name, arg))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(Ident, ResolvedGenericArg)> for GenericSubstitution {
    fn from_iter<I: IntoIterator<Item = (Ident, ResolvedGenericArg)>>(iter: I) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

/// The declaration side of one generic inference problem: the declared
/// parameters together with the already-resolved type of each `comp`
/// parameter. Resolving those types needs the analyzer, so it happens once
/// at the call site rather than inside every unification step.
pub struct GenericParams<'a> {
    pub params: &'a [HirGenericParam],
    pub comp_types: &'a [Option<CompScalarType>],
    pub pointer_bits: u32,
}

impl GenericParams<'_> {
    fn index_of(&self, name: &Ident) -> Option<usize> {
        self.params.iter().position(|p| &p.ident == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &Ident> {
        self.params.iter().map(|p| &p.ident)
    }

    /// The scalar type a `comp` parameter was declared with, or `None` when
    /// `name` is not a `comp` parameter of this declaration or its declared
    /// type failed to resolve.
    pub fn comp_type(&self, name: &Ident) -> Option<CompScalarType> {
        let index = self.index_of(name)?;
        self.params[index]
            .is_comp()
            .then(|| self.comp_types[index])?
    }

    fn is_type_param(&self, name: &Ident) -> bool {
        self.index_of(name)
            .is_some_and(|index| !self.params[index].is_comp())
    }

    fn normalize_length(&self, name: &Ident, length: u32) -> Option<ResolvedGenericArg> {
        let CompScalarType::Int(r#type) = self.comp_type(name)? else {
            return None;
        };
        let (min, max) = r#type.resolved().integer_domain(self.pointer_bits)?;
        let value = i128::from(length);
        (min..=max)
            .contains(&value)
            .then_some(ResolvedGenericArg::Comp(CompScalar::Int { r#type, value }))
    }
}

/// Binds generic parameters by matching the written type of a declaration
/// against a concrete type. Type parameters bind from the corresponding
/// concrete position; `comp` parameters bind only from compile-time
/// structural information -- currently a fixed array's length -- never from
/// a runtime value.
pub fn unify_generic_type(
    generics: &GenericParams<'_>,
    raw: &Type,
    concrete: &ResolvedType,
    subst: &mut GenericSubstitution,
) {
    match (raw, concrete) {
        (Type::Named(path), _) if path.is_unqualified() && generics.is_type_param(&path.head) => {
            subst.bind_if_absent(&path.head, || ResolvedGenericArg::Type(concrete.clone()));
        }
        (Type::Pointer(inner, _), ResolvedType::Slice { item: c, .. })
            if matches!(inner.as_ref(), Type::InferredArray(_)) =>
        {
            let Type::InferredArray(elem) = inner.as_ref() else {
                unreachable!()
            };
            unify_generic_type(generics, elem, c, subst);
        }
        (Type::Pointer(inner, _), ResolvedType::Array(c, _))
            if matches!(inner.as_ref(), Type::UnknownSizeArray(_)) =>
        {
            let Type::UnknownSizeArray(elem) = inner.as_ref() else {
                unreachable!()
            };
            unify_generic_type(generics, elem, c, subst);
        }
        (Type::Pointer(inner, _), ResolvedType::Pointer { pointee: c, .. }) => {
            unify_generic_type(generics, inner, c, subst)
        }
        (Type::SizedArray(inner, length), ResolvedType::SizedArray(c, concrete_length)) => {
            if let ArrayLength::Path(path) = length
                && path.is_unqualified()
                && generics.comp_type(&path.head).is_some()
                && let Some(arg) = generics.normalize_length(&path.head, *concrete_length)
            {
                subst.bind_if_absent(&path.head, || arg);
            }
            unify_generic_type(generics, inner, c, subst)
        }
        (Type::Function(f), ResolvedType::Function(c)) => {
            for (p, cp) in f.params.iter().zip(c.param_types()) {
                unify_generic_type(generics, &p.r#type, cp, subst);
            }
            unify_generic_type(generics, &f.return_type, &c.return_type, subst);
        }
        // `Pair<T>` against `Pair<i32>`: zips `raw`'s written arguments
        // positionally against the concrete owner's `generic_args` and
        // recurses into each pair. No check that `raw`'s path actually
        // names the same owner as `concrete` -- a wrong guess here is
        // caught afterward by the ordinary argument check.
        (Type::Generic(_, raw_args), _) => {
            let Some(concrete_args) = owner_generic_args(concrete) else {
                return;
            };
            for (r, c) in raw_args.iter().zip(&concrete_args) {
                match (r, c) {
                    (GenericArg::Type(raw), ResolvedGenericArg::Type(concrete)) => {
                        unify_generic_type(generics, raw, concrete, subst)
                    }
                    // A bare path in a value position names the `comp`
                    // parameter the concrete argument determines.
                    (GenericArg::Type(Type::Named(path)), ResolvedGenericArg::Comp(value))
                        if path.is_unqualified()
                            && generics.comp_type(&path.head) == Some(comp_type_of(value)) =>
                    {
                        subst.bind_if_absent(&path.head, || c.clone());
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn comp_type_of(value: &CompScalar) -> CompScalarType {
    match value {
        CompScalar::Int { r#type, .. } => CompScalarType::Int(*r#type),
        CompScalar::Bool(_) => CompScalarType::Bool,
        CompScalar::Char(_) => CompScalarType::Char,
    }
}

/// The argument list an inference pass produced, in declaration order.
/// Trailing parameters that stayed unbound are left off so the instantiation
/// site fills them from their own defaults; an unbound parameter without a
/// default is reported by name.
pub fn resolve_inferred_generic_args(
    generics: &GenericParams<'_>,
    subst: &GenericSubstitution,
) -> Result<Vec<ResolvedGenericArg>, Ident> {
    let mut args = Vec::with_capacity(generics.params.len());
    for param in generics.params {
        match subst.get(&param.ident) {
            Some(arg) => args.push(arg.widened()),
            None if param.default.is_some() => break,
            None => return Err(param.ident.clone()),
        }
    }
    Ok(args)
}

/// Whether `param`'s default is written as the kind its declaration binds.
pub fn default_matches_kind(param: &HirGenericParam) -> bool {
    match (&param.kind, &param.default) {
        (_, None) => true,
        // A bare path is legal for either kind; its meaning follows the
        // parameter, exactly as at a use site.
        (_, Some(GenericArg::Type(Type::Named(_)))) => true,
        (GenericParamKind::Type { .. }, Some(GenericArg::Type(_))) => true,
        (GenericParamKind::Comp { .. }, Some(GenericArg::Value(_))) => true,
        _ => false,
    }
}

fn owner_generic_args(concrete: &ResolvedType) -> Option<Vec<ResolvedGenericArg>> {
    match concrete {
        ResolvedType::Struct(cell) => Some(cell.borrow().generic_args.clone()),
        ResolvedType::Enum { cell, .. } => Some(cell.borrow().generic_args.clone()),
        ResolvedType::Union(cell) => Some(cell.borrow().generic_args.clone()),
        _ => None,
    }
}
