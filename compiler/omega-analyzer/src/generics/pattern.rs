use super::*;
use crate::resolved_type::{CallingConvention, ResolvedFunctionType};

/// A declaration's type shape. Substitution and matching never query the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypePattern {
    Fixed(ResolvedType),
    Parameter(usize),
    Pointer(Box<Self>, bool),
    Slice(Box<Self>, bool),
    Array(Box<Self>, bool),
    SizedArray(Box<Self>, ArgumentPattern),
    Nominal(Vec<Ident>, Vec<ArgumentPattern>),
    Function(Vec<Self>, Box<Self>, CallingConvention, bool),
    AnonymousEnum(Vec<Self>),
    SpecObject(Vec<SpecPattern>, bool),
}

/// One spec application a pattern names.
///
/// The declaration's `HirId` is what matching compares; the spec's own
/// declared path is carried beside it because a linker symbol needs a
/// cross-compilation identity, and it is only available here, where the
/// resolved spec is in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecPattern {
    pub spec: omega_hir::HirId,
    pub module_path: Vec<Ident>,
    pub name: Ident,
    pub args: Vec<ArgumentPattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentPattern {
    Type(Box<TypePattern>),
    Value(CompScalar),
    Parameter(usize),
}

impl TypePattern {
    pub fn infer(&self, found: &ResolvedType, bindings: &mut [Option<ResolvedGenericArg>]) {
        match (self, found) {
            (Self::Parameter(index), _) => {
                bindings[*index].get_or_insert_with(|| ResolvedGenericArg::Type(found.widened()));
            }
            (Self::Pointer(inner, _), ResolvedType::Pointer { pointee, .. }) => {
                inner.infer(pointee, bindings)
            }
            (Self::Slice(inner, _), ResolvedType::Slice { item, .. }) => {
                inner.infer(item, bindings)
            }
            (Self::Array(inner, _), ResolvedType::Array(item, _)) => inner.infer(item, bindings),
            (Self::SizedArray(inner, length), ResolvedType::SizedArray(item, size)) => {
                inner.infer(item, bindings);
                if let ArgumentPattern::Parameter(index) = length {
                    bindings[*index].get_or_insert(ResolvedGenericArg::Comp(CompScalar::Int {
                        r#type: crate::resolved_type::CompIntType::USize,
                        value: i128::from(*size),
                    }));
                }
            }
            (Self::Nominal(path, args), _) => {
                if let Some((found_path, found_args)) = nominal(found)
                    && *path == found_path
                {
                    for (pattern, arg) in args.iter().zip(&found_args) {
                        pattern.infer(arg, bindings);
                    }
                }
            }
            (Self::Function(params, result, _, _), ResolvedType::Function(function)) => {
                for (pattern, found) in params.iter().zip(function.param_types()) {
                    pattern.infer(found, bindings);
                }
                result.infer(&function.return_type, bindings);
            }
            _ => {}
        }
    }

    pub fn resolved(&self, bindings: &[Option<ResolvedGenericArg>]) -> Option<ResolvedType> {
        Some(match self {
            Self::Fixed(ty) => ty.clone(),
            Self::Parameter(index) => bindings.get(*index)?.as_ref()?.as_type()?.clone(),
            Self::Pointer(inner, mutable) => ResolvedType::Pointer {
                pointee: Box::new(inner.resolved(bindings)?),
                mutable: *mutable,
            },
            Self::Slice(inner, mutable) => ResolvedType::Slice {
                item: Box::new(inner.resolved(bindings)?),
                mutable: *mutable,
            },
            Self::Array(inner, mutable) => {
                ResolvedType::Array(Box::new(inner.resolved(bindings)?), *mutable)
            }
            Self::SizedArray(inner, length) => ResolvedType::SizedArray(
                Box::new(inner.resolved(bindings)?),
                length.length(bindings)?,
            ),
            Self::Function(params, result, convention, variadic) => {
                ResolvedType::Function(ResolvedFunctionType {
                    params: params
                        .iter()
                        .map(|p| {
                            Some(crate::resolved_type::ResolvedFunctionParam::anonymous(
                                p.resolved(bindings)?,
                            ))
                        })
                        .collect::<Option<_>>()?,
                    return_type: Box::new(result.resolved(bindings)?),
                    calling_convention: *convention,
                    is_variadic: *variadic,
                    self_mode: None,
                })
            }
            Self::AnonymousEnum(members) => ResolvedType::AnonymousEnum {
                shape: std::rc::Rc::new(crate::resolved_type::ResolvedAnonymousEnum::canonicalize(
                    members
                        .iter()
                        .map(|p| p.resolved(bindings))
                        .collect::<Option<_>>()?,
                )),
                variant: None,
            },
            Self::Nominal(_, _) | Self::SpecObject(_, _) => return None,
        })
    }

    pub fn leaves(&self, bindings: &[Option<ResolvedGenericArg>]) -> Vec<Self> {
        match self {
            Self::AnonymousEnum(members) => members
                .iter()
                .flat_map(|member| member.leaves(bindings))
                .collect(),
            _ => match self.resolved(bindings) {
                Some(ResolvedType::AnonymousEnum { shape, .. }) => {
                    shape.members().iter().cloned().map(Self::Fixed).collect()
                }
                _ => vec![self.clone()],
            },
        }
    }

    pub fn exact(&self, found: &ResolvedType, bindings: &[Option<ResolvedGenericArg>]) -> bool {
        if let Some(expected) = self.resolved(bindings) {
            return expected == *found;
        }
        match (self, found) {
            (
                Self::Nominal(_, _),
                ResolvedType::Enum {
                    variant: Some(_), ..
                },
            )
            | (
                Self::AnonymousEnum(_),
                ResolvedType::AnonymousEnum {
                    variant: Some(_), ..
                },
            ) => false,
            (
                Self::Pointer(inner, mutable),
                ResolvedType::Pointer {
                    pointee,
                    mutable: found_mut,
                },
            )
            | (
                Self::Slice(inner, mutable),
                ResolvedType::Slice {
                    item: pointee,
                    mutable: found_mut,
                },
            ) => mutable == found_mut && inner.exact(pointee, bindings),
            (Self::Array(inner, mutable), ResolvedType::Array(item, found_mut)) => {
                mutable == found_mut && inner.exact(item, bindings)
            }
            _ => self.accepts(found, bindings),
        }
    }

    /// Whether `found` is *the* type this pattern denotes under `bindings`,
    /// with no representation acceptance anywhere inside it.
    ///
    /// [`Self::exact`] is a matching predicate for a call, so it ends in
    /// [`Self::accepts`] for the shapes a substitution cannot materialize.
    /// Selecting a function *value* has no conversion to pay for the
    /// difference: a nominal argument, a nested function type, an
    /// anonymous-enum member set, and a pointer's mutability must all be
    /// identical, at every depth. An unbound parameter denotes nothing, so it
    /// matches nothing.
    pub fn identical(&self, found: &ResolvedType, bindings: &[Option<ResolvedGenericArg>]) -> bool {
        if let Some(expected) = self.resolved(bindings) {
            return expected == *found;
        }
        match (self, found) {
            (Self::Nominal(path, args), _) => {
                nominal(found).is_some_and(|(found_path, found_args)| {
                    *path == found_path
                        && args.len() == found_args.len()
                        && args
                            .iter()
                            .zip(&found_args)
                            .all(|(pattern, arg)| pattern.identical(arg, bindings))
                })
            }
            (
                Self::SpecObject(members, mutable),
                ResolvedType::SpecObject {
                    shape,
                    mutable: found_mut,
                },
            ) => {
                mutable == found_mut
                    && members.len() == shape.members.len()
                    && members.iter().all(|spec| {
                        shape.members.iter().any(|member| {
                            member.spec.borrow().id == spec.spec
                                && spec.args.len() == member.spec_args.len()
                                && spec
                                    .args
                                    .iter()
                                    .zip(&member.spec_args)
                                    .all(|(pattern, found)| pattern.identical(found, bindings))
                        })
                    })
            }
            (
                Self::Function(params, result, convention, variadic),
                ResolvedType::Function(found),
            ) => {
                *convention == found.calling_convention
                    && *variadic == found.is_variadic
                    && found.self_mode.is_none()
                    && params.len() == found.params.len()
                    && params
                        .iter()
                        .zip(found.param_types())
                        .all(|(pattern, found)| pattern.identical(found, bindings))
                    && result.identical(&found.return_type, bindings)
            }
            (
                Self::AnonymousEnum(_),
                ResolvedType::AnonymousEnum {
                    shape,
                    variant: None,
                },
            ) => {
                let leaves = self.leaves(bindings);
                leaves.iter().all(|pattern| {
                    shape
                        .members()
                        .iter()
                        .any(|found| pattern.identical(found, bindings))
                }) && shape.members().iter().all(|found| {
                    leaves
                        .iter()
                        .any(|pattern| pattern.identical(found, bindings))
                })
            }
            (
                Self::Pointer(inner, mutable),
                ResolvedType::Pointer {
                    pointee,
                    mutable: found_mut,
                },
            )
            | (
                Self::Slice(inner, mutable),
                ResolvedType::Slice {
                    item: pointee,
                    mutable: found_mut,
                },
            ) => mutable == found_mut && inner.identical(pointee, bindings),
            (Self::Array(inner, mutable), ResolvedType::Array(item, found_mut)) => {
                mutable == found_mut && inner.identical(item, bindings)
            }
            (Self::SizedArray(inner, length), ResolvedType::SizedArray(item, size)) => {
                length.length(bindings) == Some(*size) && inner.identical(item, bindings)
            }
            _ => false,
        }
    }

    pub fn accepts(&self, found: &ResolvedType, bindings: &[Option<ResolvedGenericArg>]) -> bool {
        if let Some(expected) = self.resolved(bindings) {
            return expected.accepts(found);
        }
        match (self, found) {
            (Self::Nominal(path, args), _) => {
                nominal(found).is_some_and(|(found_path, found_args)| {
                    *path == found_path
                        && args.len() == found_args.len()
                        && args
                            .iter()
                            .zip(&found_args)
                            .all(|(p, a)| p.matches(a, bindings))
                })
            }
            (
                Self::SpecObject(members, mutable),
                ResolvedType::SpecObject {
                    shape,
                    mutable: found_mut,
                },
            ) => {
                mutable == found_mut
                    && members.len() == shape.members.len()
                    && members.iter().all(|spec| {
                        shape.members.iter().any(|member| {
                            member.spec.borrow().id == spec.spec
                                && spec.args.len() == member.spec_args.len()
                                && spec
                                    .args
                                    .iter()
                                    .zip(&member.spec_args)
                                    .all(|(pattern, found)| pattern.matches(found, bindings))
                        })
                    })
            }
            (
                Self::Function(params, result, convention, variadic),
                ResolvedType::Function(found),
            ) => {
                *convention == found.calling_convention
                    && *variadic == found.is_variadic
                    && found.self_mode.is_none()
                    && params.len() == found.params.len()
                    && params
                        .iter()
                        .zip(found.param_types())
                        .all(|(pattern, found)| pattern.exact(found, bindings))
                    && result.exact(&found.return_type, bindings)
            }
            (Self::AnonymousEnum(_), ResolvedType::AnonymousEnum { shape, .. }) => {
                let leaves = self.leaves(bindings);
                leaves.iter().all(|pattern| {
                    shape
                        .members()
                        .iter()
                        .any(|found| pattern.exact(found, bindings))
                }) && shape
                    .members()
                    .iter()
                    .all(|found| leaves.iter().any(|pattern| pattern.exact(found, bindings)))
            }
            (
                Self::Pointer(inner, mutable),
                ResolvedType::Pointer {
                    pointee,
                    mutable: found_mut,
                },
            )
            | (
                Self::Slice(inner, mutable),
                ResolvedType::Slice {
                    item: pointee,
                    mutable: found_mut,
                },
            ) => {
                (!mutable && inner.accepts(pointee, bindings))
                    || (*mutable && *found_mut && inner.exact(pointee, bindings))
            }
            (Self::Array(inner, mutable), ResolvedType::Array(item, found_mut)) => {
                (!mutable && inner.accepts(item, bindings))
                    || (*mutable && *found_mut && inner.exact(item, bindings))
            }
            (Self::SizedArray(inner, length), ResolvedType::SizedArray(item, size)) => {
                length.length(bindings) == Some(*size) && inner.exact(item, bindings)
            }
            _ => false,
        }
    }
}

impl ArgumentPattern {
    fn infer(&self, found: &ResolvedGenericArg, bindings: &mut [Option<ResolvedGenericArg>]) {
        match (self, found) {
            (Self::Type(pattern), ResolvedGenericArg::Type(found)) => {
                pattern.infer(found, bindings)
            }
            (Self::Parameter(index), _) => {
                bindings[*index].get_or_insert_with(|| found.clone());
            }
            _ => {}
        }
    }

    fn identical(
        &self,
        found: &ResolvedGenericArg,
        bindings: &[Option<ResolvedGenericArg>],
    ) -> bool {
        match (self, found) {
            (Self::Type(pattern), ResolvedGenericArg::Type(found)) => {
                pattern.identical(found, bindings)
            }
            (Self::Value(value), ResolvedGenericArg::Comp(found)) => value == found,
            (Self::Parameter(index), _) => bindings[*index].as_ref() == Some(found),
            _ => false,
        }
    }

    fn matches(&self, found: &ResolvedGenericArg, bindings: &[Option<ResolvedGenericArg>]) -> bool {
        match (self, found) {
            (Self::Type(pattern), ResolvedGenericArg::Type(found)) => {
                pattern.exact(found, bindings)
            }
            (Self::Value(value), ResolvedGenericArg::Comp(found)) => value == found,
            (Self::Parameter(index), _) => bindings[*index].as_ref() == Some(found),
            _ => false,
        }
    }

    fn length(&self, bindings: &[Option<ResolvedGenericArg>]) -> Option<u32> {
        let value = match self {
            Self::Value(value) => value,
            Self::Parameter(index) => match bindings.get(*index)?.as_ref()? {
                ResolvedGenericArg::Comp(value) => value,
                _ => return None,
            },
            Self::Type(_) => return None,
        };
        match value {
            CompScalar::Int { value, .. } => (*value).try_into().ok(),
            _ => None,
        }
    }
}

fn nominal(ty: &ResolvedType) -> Option<(Vec<Ident>, Vec<ResolvedGenericArg>)> {
    let (module, name, args) = match ty {
        ResolvedType::Struct(cell) => {
            let t = cell.borrow();
            (
                t.module_path.clone(),
                t.name.clone(),
                t.generic_args.clone(),
            )
        }
        ResolvedType::Union(cell) => {
            let t = cell.borrow();
            (
                t.module_path.clone(),
                t.name.clone(),
                t.generic_args.clone(),
            )
        }
        ResolvedType::Enum { cell, .. } => {
            let t = cell.borrow();
            (
                t.module_path.clone(),
                t.name.clone(),
                t.generic_args.clone(),
            )
        }
        _ => return None,
    };
    Some((
        module.into_iter().chain(std::iter::once(name)).collect(),
        args,
    ))
}
