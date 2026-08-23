use omega_analyzer::resolved_type::{CallingConvention, ResolvedFunctionType, ResolvedType};
use omega_mangle::{FunctionSignature, MangleConvention, ManglePath, MangleType, Namespace};
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
    type_args: &[ResolvedType],
) -> ManglePath {
    let path = ManglePath::Nested(
        Box::new(module_path(module)),
        Namespace::Type,
        name.as_ref().to_owned(),
    );
    generic_path(path, type_args)
}

pub(super) fn generic_path(path: ManglePath, type_args: &[ResolvedType]) -> ManglePath {
    if type_args.is_empty() {
        path
    } else {
        ManglePath::Generic(Box::new(path), type_args.iter().map(mangle_type).collect())
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
            named_type(&cell.module_path, &cell.name, &cell.type_args, None)
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            named_type(&cell.module_path, &cell.name, &cell.type_args, None)
        }
        ResolvedType::Enum { cell, variant } => {
            let cell = cell.borrow();
            let variant = (*variant).map(|index| {
                u32::try_from(index)
                    .expect("omega-mangle cannot represent enum variant indices above u32::MAX")
            });
            named_type(&cell.module_path, &cell.name, &cell.type_args, variant)
        }
        ResolvedType::Spec(cell) => {
            let cell = cell.borrow();
            named_type(&cell.module_path, &cell.name, &cell.type_args, None)
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
    }
}

pub(super) fn signature(function: &ResolvedFunctionType) -> FunctionSignature {
    FunctionSignature {
        params: function
            .params
            .iter()
            .map(|(_, r#type)| mangle_type(r#type))
            .collect(),
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
    type_args: &[ResolvedType],
    variant: Option<u32>,
) -> MangleType {
    MangleType::Named(nominal_path(module, name, type_args), variant)
}
