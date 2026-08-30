use omega_analyzer::resolved_type::{
    CallingConvention, CompIntType, CompScalar, ResolvedFunctionType, ResolvedGenericArg,
    ResolvedType,
};
use omega_mangle::{
    FunctionSignature, MangleConvention, MangleGenericArg, MangleIntType, ManglePath, MangleType,
    MangleValue, Namespace,
};
use omega_parser::prelude::Ident;

fn mangle_convention(convention: CallingConvention) -> MangleConvention {
    match convention {
        CallingConvention::Omega => MangleConvention::Omega,
        CallingConvention::C => MangleConvention::C,
        CallingConvention::SysV64 => MangleConvention::SysV64,
    }
}

pub(super) fn module_path(segments: &[Ident]) -> ManglePath {
    let (root, nested) = segments
        .split_first()
        .expect("checked module paths are never empty");
    nested.iter().fold(
        ManglePath::Root(root.as_ref().to_owned()),
        |parent, segment| {
            ManglePath::Nested(
                Box::new(parent),
                Namespace::Type,
                segment.as_ref().to_owned(),
            )
        },
    )
}

pub(super) fn nominal_path(
    module: &[Ident],
    name: &Ident,
    generic_args: &[ResolvedGenericArg],
) -> ManglePath {
    let path = ManglePath::Nested(
        Box::new(module_path(module)),
        Namespace::Type,
        name.as_ref().to_owned(),
    );
    generic_path(path, generic_args)
}

/// An all-type argument list keeps the pre-existing `Generic` encoding
/// byte-for-byte; only a compile-time value argument switches to the mixed
/// form.
pub(super) fn generic_path(path: ManglePath, generic_args: &[ResolvedGenericArg]) -> ManglePath {
    if generic_args.is_empty() {
        path
    } else {
        ManglePath::generic(path, generic_args.iter().map(mangle_generic_arg).collect())
    }
}

pub(super) fn mangle_generic_arg(arg: &ResolvedGenericArg) -> MangleGenericArg {
    match arg {
        ResolvedGenericArg::Type(r#type) => MangleGenericArg::Type(mangle_type(r#type)),
        ResolvedGenericArg::Comp(value) => MangleGenericArg::Value(mangle_value(value)),
    }
}

fn mangle_value(value: &CompScalar) -> MangleValue {
    match value {
        CompScalar::Int { r#type, value } => MangleValue::Int {
            r#type: mangle_int_type(*r#type),
            value: *value,
        },
        CompScalar::Bool(value) => MangleValue::Bool(*value),
        CompScalar::Char(value) => MangleValue::Char(*value),
    }
}

fn mangle_int_type(r#type: CompIntType) -> MangleIntType {
    match r#type {
        CompIntType::I8 => MangleIntType::I8,
        CompIntType::I16 => MangleIntType::I16,
        CompIntType::I32 => MangleIntType::I32,
        CompIntType::I64 => MangleIntType::I64,
        CompIntType::ISize => MangleIntType::ISize,
        CompIntType::U8 => MangleIntType::U8,
        CompIntType::U16 => MangleIntType::U16,
        CompIntType::U32 => MangleIntType::U32,
        CompIntType::U64 => MangleIntType::U64,
        CompIntType::USize => MangleIntType::USize,
    }
}

pub(super) fn mangle_type(ty: &ResolvedType) -> MangleType {
    match ty {
        ResolvedType::Void => MangleType::Void,
        ResolvedType::Never => MangleType::Never,
        ResolvedType::Bool => MangleType::Bool,
        ResolvedType::Char => MangleType::Char,
        ResolvedType::I8 => MangleType::I8,
        ResolvedType::I16 => MangleType::I16,
        ResolvedType::I32 => MangleType::I32,
        ResolvedType::I64 => MangleType::I64,
        ResolvedType::ISize => MangleType::ISize,
        ResolvedType::U8 => MangleType::U8,
        ResolvedType::U16 => MangleType::U16,
        ResolvedType::U32 => MangleType::U32,
        ResolvedType::U64 => MangleType::U64,
        ResolvedType::USize => MangleType::USize,
        ResolvedType::F32 => MangleType::F32,
        ResolvedType::F64 => MangleType::F64,
        ResolvedType::Pointer { pointee, mutable } => {
            MangleType::Pointer(Box::new(mangle_type(pointee)), *mutable)
        }
        ResolvedType::Slice { item, mutable } => {
            MangleType::Slice(Box::new(mangle_type(item)), *mutable)
        }
        ResolvedType::Str { mutable } => MangleType::Str(*mutable),
        ResolvedType::Array(item, mutable) => {
            MangleType::Array(Box::new(mangle_type(item)), *mutable)
        }
        ResolvedType::SizedArray(item, len) => {
            MangleType::SizedArray(Box::new(mangle_type(item)), u64::from(*len))
        }
        ResolvedType::Function(function) => {
            let sig = signature(function);
            MangleType::Function(
                sig.params,
                Box::new(sig.return_type),
                sig.is_variadic,
                sig.convention,
            )
        }
        ResolvedType::Struct(cell) => {
            let cell = cell.borrow();
            named_type(&cell.module_path, &cell.name, &cell.generic_args, None)
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            named_type(&cell.module_path, &cell.name, &cell.generic_args, None)
        }
        ResolvedType::Enum { cell, variant } => {
            let cell = cell.borrow();
            let variant = (*variant).map(|index| {
                u32::try_from(index)
                    .expect("omega-mangle cannot represent enum variant indices above u32::MAX")
            });
            named_type(&cell.module_path, &cell.name, &cell.generic_args, variant)
        }
        ResolvedType::Spec(cell) => {
            let cell = cell.borrow();
            named_type(&cell.module_path, &cell.name, &cell.generic_args, None)
        }
        ResolvedType::SpecObject { shape, mutable } => {
            // `shape.members` is already in canonical final-name order, so
            // this never needs to re-sort: source permutations already
            // produced the same member order here.
            let members = shape
                .members
                .iter()
                .map(|member| {
                    let spec = member.spec.borrow();
                    named_type(&spec.module_path, &spec.name, &member.spec_args, None)
                })
                .collect();
            MangleType::SpecObject(members, *mutable)
        }
        ResolvedType::AnonymousEnum { shape, variant } => {
            // Already canonically ordered by the analyzer; re-sorting here
            // would be a second, competing notion of member order.
            let members = shape.members().iter().map(mangle_type).collect();
            let variant = (*variant).map(|index| {
                u32::try_from(index)
                    .expect("omega-mangle cannot represent member indices above u32::MAX")
            });
            MangleType::AnonymousEnum(members, variant)
        }
    }
}

pub(super) fn signature(function: &ResolvedFunctionType) -> FunctionSignature {
    FunctionSignature {
        params: function.param_types().map(mangle_type).collect(),
        return_type: mangle_type(&function.return_type),
        is_variadic: function.is_variadic,
        convention: mangle_convention(function.calling_convention),
    }
}

pub(super) fn owner_path(target: &ResolvedType) -> ManglePath {
    match mangle_type(target) {
        MangleType::Named(path, _) => path,
        target => ManglePath::Type(Box::new(target)),
    }
}

fn named_type(
    module: &[Ident],
    name: &Ident,
    generic_args: &[ResolvedGenericArg],
    variant: Option<u32>,
) -> MangleType {
    MangleType::Named(nominal_path(module, name, generic_args), variant)
}
