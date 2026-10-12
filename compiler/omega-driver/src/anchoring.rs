//! Declaration syntax handed to a use site in another module.
//!
//! A generic signature crosses the resolver interface as written syntax,
//! because the call site still has to infer the arguments it is instantiated
//! with. Its names must nevertheless keep meaning what they mean where the
//! declaration is written, so every path is given its declaring module's
//! definition origin before it leaves the driver. A path that already carries
//! an origin -- macro-authored syntax -- keeps the one it has.

use omega_hir::HirGenericParam;
use omega_parser::prelude::{
    ArrayLength, FunctionType, FunctionTypeParam, GenericArg, GenericParamKind, Origin, Path, Type,
};

pub(crate) fn anchor_type(r#type: &Type, origin: Origin) -> Type {
    let recur = |inner: &Type| anchor_type(inner, origin);
    match r#type {
        Type::Named(path) => Type::Named(anchor_path(path, origin)),
        Type::Generic(path, args) => Type::Generic(
            anchor_path(path, origin),
            args.iter().map(|arg| anchor_arg(arg, origin)).collect(),
        ),
        Type::Pointer(inner, mutable) => Type::Pointer(Box::new(recur(inner)), *mutable),
        Type::InferredArray(inner) => Type::InferredArray(Box::new(recur(inner))),
        Type::UnknownSizeArray(inner) => Type::UnknownSizeArray(Box::new(recur(inner))),
        Type::SizedArray(inner, length) => {
            let length = match length {
                ArrayLength::Path(path) => ArrayLength::Path(anchor_path(path, origin)),
                ArrayLength::Literal(_) | ArrayLength::Infer => length.clone(),
            };
            Type::SizedArray(Box::new(recur(inner)), length)
        }
        Type::SpecStatic(members) => Type::SpecStatic(members.iter().map(recur).collect()),
        Type::AnonymousEnum(members) => Type::AnonymousEnum(members.iter().map(recur).collect()),
        Type::Function(function) => Type::Function(FunctionType {
            params: function
                .params
                .iter()
                .map(|param| FunctionTypeParam {
                    r#type: recur(&param.r#type),
                    ..param.clone()
                })
                .collect(),
            return_type: Box::new(recur(&function.return_type)),
            ..function.clone()
        }),
        Type::Infer => Type::Infer,
    }
}

pub(crate) fn anchor_generics(
    generics: &[HirGenericParam],
    origin: Origin,
) -> Vec<HirGenericParam> {
    generics
        .iter()
        .map(|param| HirGenericParam {
            ident: param.ident.clone(),
            kind: match &param.kind {
                GenericParamKind::Type { bounds } => GenericParamKind::Type {
                    bounds: bounds.iter().map(|b| anchor_type(b, origin)).collect(),
                },
                GenericParamKind::Comp { value_type } => GenericParamKind::Comp {
                    value_type: anchor_type(value_type, origin),
                },
            },
            default: param.default.as_ref().map(|arg| anchor_arg(arg, origin)),
        })
        .collect()
}

fn anchor_arg(arg: &GenericArg, origin: Origin) -> GenericArg {
    match arg {
        GenericArg::Type(inner) => GenericArg::Type(anchor_type(inner, origin)),
        GenericArg::Value(_) | GenericArg::Infer => arg.clone(),
    }
}

fn anchor_path(path: &Path, origin: Origin) -> Path {
    if path.origin != Origin::default() {
        return path.clone();
    }
    Path {
        origin,
        ..path.clone()
    }
}
