use crate::resolved_type::{CompScalar, ResolvedGenericArg, ResolvedType};
use crate::resolver::{Names, ResolveError};
use omega_hir::{HirFunctionDef, HirGenericParam, HirId, ItemPath, ModulePath};
use omega_parser::prelude::{GenericArg, GenericParamKind, Ident, Type};

pub mod pattern;

#[cfg(test)]
mod tests;

pub fn compare_bound_sets<T: PartialEq>(
    candidate: &[T],
    incumbent: &[T],
) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (
        candidate.iter().all(|bound| incumbent.contains(bound)),
        incumbent.iter().all(|bound| candidate.contains(bound)),
    ) {
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        (true, true) => Some(Ordering::Equal),
        (false, false) => None,
    }
}

/// The identity of one declared generic parameter: the declaration that
/// declares it, and its position in that declaration's list.
///
/// Signatures, inference, and substitution identify a parameter by this, never
/// by its name; a name only resolves lexical references inside the
/// declaration, so two parameters that spell the same name stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenericParamId {
    pub owner: HirId,
    pub index: u32,
}

/// Where a generic parameter came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericParamOrigin {
    /// Written in the declaration's generic parameter list.
    Written,
    /// Introduced by a `spec S` parameter type at this parameter index. Such
    /// a parameter has no name: only that parameter's own type refers to it.
    StaticSpecParameter { param_index: usize },
}

/// One declared generic parameter.
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub id: GenericParamId,
    pub name: Option<Ident>,
    pub origin: GenericParamOrigin,
    pub kind: GenericParamKind,
    pub default: Option<GenericArg>,
}

impl GenericParam {
    /// The written parameters `owner` declares, in order.
    pub fn declared(owner: HirId, params: &[HirGenericParam]) -> Vec<Self> {
        params
            .iter()
            .enumerate()
            .map(|(index, param)| Self {
                id: GenericParamId {
                    owner,
                    index: index as u32,
                },
                name: Some(param.ident.clone()),
                origin: GenericParamOrigin::Written,
                kind: param.kind.clone(),
                default: param.default.clone(),
            })
            .collect()
    }

    pub fn bounds(&self) -> &[Type] {
        match &self.kind {
            GenericParamKind::Type { bounds } => bounds,
            GenericParamKind::Comp { .. } => &[],
        }
    }

    pub fn comp_type(&self) -> Option<&Type> {
        match &self.kind {
            GenericParamKind::Comp { value_type } => Some(value_type),
            GenericParamKind::Type { .. } => None,
        }
    }

    pub fn is_comp(&self) -> bool {
        matches!(self.kind, GenericParamKind::Comp { .. })
    }

    /// Whether a written reference to `name` inside the declaration means
    /// this parameter.
    pub fn is_named(&self, name: &Ident) -> bool {
        self.name.as_ref() == Some(name)
    }

    /// How a diagnostic names this parameter: its declared name, or for an
    /// anonymous parameter the `spec` bound it was written as.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) => name.to_string(),
            None => format!(
                "spec {}",
                self.bounds()
                    .iter()
                    .map(crate::error::raw_type_display)
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
        }
    }
}

/// A function together with its effective generic parameters: the written
/// ones, then one anonymous parameter per outermost static `spec ...`
/// parameter type.
#[derive(Debug, Clone)]
pub struct NormalizedFunction {
    pub function: HirFunctionDef,
    pub generics: Vec<GenericParam>,
}

impl NormalizedFunction {
    /// The generic position whose binding is parameter `param_index`'s type,
    /// when that parameter was written as `spec ...`.
    pub fn static_spec_position(&self, param_index: usize) -> Option<usize> {
        self.generics.iter().position(|generic| {
            generic.origin == GenericParamOrigin::StaticSpecParameter { param_index }
        })
    }
}

/// Gives each outermost static `spec ...` parameter its own anonymous
/// bounded generic, after expanding aliases so that `f(x: spec A + B)` and
/// `f(x: AB)` normalize identically. Only a parameter's outermost type
/// participates: `*spec A + B` is a dynamic object, and a spec buried in an
/// array, generic argument, or function type is not a parameter bound at all.
///
/// This is the one place the rule lives; every query that reports a function's
/// generics, signature, or body works from the result.
pub fn normalize_function<R: Names + ?Sized>(
    resolver: &mut R,
    module_path: &ModulePath,
    f: &HirFunctionDef,
) -> Result<NormalizedFunction, ResolveError> {
    let mut generics = GenericParam::declared(f.id, &f.generics);
    for (param_index, param) in f.params.iter().enumerate() {
        let expanded =
            crate::aliases::expand_type_alias(resolver, module_path, param.r#type.clone())?;
        let Type::SpecStatic(members) = expanded else {
            continue;
        };
        generics.push(GenericParam {
            id: GenericParamId {
                owner: f.id,
                index: generics.len() as u32,
            },
            name: None,
            origin: GenericParamOrigin::StaticSpecParameter { param_index },
            kind: GenericParamKind::Type { bounds: members },
            default: None,
        });
    }
    Ok(NormalizedFunction {
        function: f.clone(),
        generics,
    })
}

/// One parameter bound by an instantiation. The name is how the binding
/// enters the declaration's lexical scope; it plays no part in identity.
#[derive(Debug, Clone, PartialEq)]
pub struct GenericBinding {
    pub param: GenericParamId,
    pub name: Option<Ident>,
    pub arg: ResolvedGenericArg,
}

/// The bindings one generic instantiation installs while the instantiated
/// declaration is analyzed, together with the `Self` type of an owner
/// instantiation. Later bindings shadow earlier ones that share a name, so an
/// enclosing owner's bindings come first and a member's own come after.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenericSubstitution {
    bindings: Vec<GenericBinding>,
    self_type: Option<ResolvedType>,
}

impl GenericSubstitution {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pairs declared parameters with the arguments an instantiation
    /// resolved for them. Surplus on either side is ignored; argument count
    /// is checked where the instantiation is formed.
    pub fn zip(params: &[GenericParam], args: &[ResolvedGenericArg]) -> Self {
        let mut substitution = Self::new();
        for (param, arg) in params.iter().zip(args) {
            substitution.bind(param, arg.clone());
        }
        substitution
    }

    /// The same for positional bindings, skipping the positions still open.
    pub fn from_bindings(params: &[GenericParam], args: &[Option<ResolvedGenericArg>]) -> Self {
        let mut substitution = Self::new();
        for (param, arg) in params.iter().zip(args) {
            if let Some(arg) = arg {
                substitution.bind(param, arg.clone());
            }
        }
        substitution
    }

    pub fn bind(&mut self, param: &GenericParam, arg: ResolvedGenericArg) {
        self.bindings.push(GenericBinding {
            param: param.id,
            name: param.name.clone(),
            arg,
        });
    }

    pub fn with_self_type(mut self, self_type: ResolvedType) -> Self {
        self.self_type = Some(self_type);
        self
    }

    pub fn self_type(&self) -> Option<&ResolvedType> {
        self.self_type.as_ref()
    }

    /// `own` analyzed inside `self`: the enclosing bindings first, so a name
    /// `own` redeclares shadows the enclosing one.
    pub fn enclosing(&self, own: &Self) -> Self {
        let mut combined = self.clone();
        combined.bindings.extend(own.bindings.iter().cloned());
        if own.self_type.is_some() {
            combined.self_type.clone_from(&own.self_type);
        }
        combined
    }

    pub fn get(&self, param: GenericParamId) -> Option<&ResolvedGenericArg> {
        self.bindings
            .iter()
            .rev()
            .find_map(|binding| (binding.param == param).then_some(&binding.arg))
    }

    pub fn bindings(&self) -> impl Iterator<Item = &GenericBinding> {
        self.bindings.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty() && self.self_type.is_none()
    }
}

/// The argument list an inference pass produced, in declaration order. An
/// unbound parameter with a default is a gap (`None`) at any position, which
/// the instantiation site fills from that default; an unbound parameter
/// without one is reported by its position.
pub fn resolve_inferred_generic_args(
    generics: &[GenericParam],
    bindings: &[Option<ResolvedGenericArg>],
) -> Result<Vec<Option<ResolvedGenericArg>>, usize> {
    generics
        .iter()
        .enumerate()
        .map(
            |(position, param)| match bindings.get(position).cloned().flatten() {
                Some(arg) => Ok(Some(arg.widened())),
                None if param.default.is_some() => Ok(None),
                None => Err(position),
            },
        )
        .collect()
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
