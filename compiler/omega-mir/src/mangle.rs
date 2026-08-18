
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mangle::{ManglePath, MangleType, Namespace, Symbol};
use omega_parser::prelude::Ident;

fn mangle_module_path(segments: &[Ident]) -> ManglePath {
    let mut iter = segments.iter();
    let first = iter.next().expect("a module path is never empty");
    let mut path = ManglePath::Root(first.as_ref().to_string());
    for seg in iter {
        // Intermediate module path segments use the fixed namespace discriminator expected by the mangling grammar.
        path = ManglePath::Nested(Box::new(path), Namespace::Type, seg.as_ref().to_string());
    }
    path
}

fn mangle_type_path(module_path: &[Ident], name: &Ident, type_args: &[ResolvedType]) -> ManglePath {
    let base = ManglePath::Nested(
        Box::new(mangle_module_path(module_path)),
        Namespace::Type,
        name.as_ref().to_string(),
    );
    if type_args.is_empty() {
        base
    } else {
        ManglePath::Generic(Box::new(base), type_args.iter().map(mangle_type).collect())
    }
}

fn mangle_type(ty: &ResolvedType) -> MangleType {
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
        ResolvedType::Array(inner, mutable) => {
            MangleType::Array(Box::new(mangle_type(inner)), *mutable)
        }
        ResolvedType::SizedArray(inner, len) => {
            MangleType::SizedArray(Box::new(mangle_type(inner)), u64::from(*len))
        }
        ResolvedType::Function(fn_type) => {
            let (params, ret) = build_signature(fn_type);
            MangleType::Function(params, Box::new(ret), fn_type.is_variadic)
        }
        ResolvedType::Struct(cell) => {
            let cell = cell.borrow();
            MangleType::Named(
                mangle_type_path(&cell.module_path, &cell.name, &cell.type_args),
                None,
            )
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            MangleType::Named(
                mangle_type_path(&cell.module_path, &cell.name, &cell.type_args),
                None,
            )
        }
        ResolvedType::Enum { cell, variant } => {
            let cell = cell.borrow();
            let variant = variant.map(|v| v as u32);
            MangleType::Named(
                mangle_type_path(&cell.module_path, &cell.name, &cell.type_args),
                variant,
            )
        }
        ResolvedType::Spec(cell) => {
            let cell = cell.borrow();
            MangleType::Named(
                mangle_type_path(&cell.module_path, &cell.name, &cell.type_args),
                None,
            )
        }
        ResolvedType::SpecObject {
            spec,
            type_args,
            mutable,
        } => {
            let cell = spec.borrow();
            let inner = MangleType::Named(
                mangle_type_path(&cell.module_path, &cell.name, type_args),
                None,
            );
            MangleType::SpecObject(Box::new(inner), *mutable)
        }
    }
}

fn build_signature(fn_type: &ResolvedFunctionType) -> (Vec<MangleType>, MangleType) {
    (
        fn_type.params.iter().map(|(_, t)| mangle_type(t)).collect(),
        mangle_type(&fn_type.return_type),
    )
}

pub fn free_function_symbol(
    module_path: &[Ident],
    name: &Ident,
    type_args: &[ResolvedType],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let leaf = ManglePath::Nested(
        Box::new(mangle_module_path(module_path)),
        Namespace::Value,
        name.as_ref().to_string(),
    );
    let path = if type_args.is_empty() {
        leaf
    } else {
        ManglePath::Generic(Box::new(leaf), type_args.iter().map(mangle_type).collect())
    };
    let (params, ret) = build_signature(fn_type);
    Symbol {
        path,
        signature: Some((params, ret)),
        vendor_suffix: None,
    }
}

pub fn global_symbol(module_path: &[Ident], name: &Ident) -> Symbol {
    let path = ManglePath::Nested(
        Box::new(mangle_module_path(module_path)),
        Namespace::Value,
        name.as_ref().to_string(),
    );
    Symbol {
        path,
        signature: None,
        vendor_suffix: None,
    }
}

pub fn method_symbol(
    module_path: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let owner = mangle_type_path(module_path, owner_name, owner_type_args);
    let path = ManglePath::Nested(
        Box::new(owner),
        Namespace::Value,
        method_name.as_ref().to_string(),
    );
    let (params, ret) = build_signature(fn_type);
    Symbol {
        path,
        signature: Some((params, ret)),
        vendor_suffix: None,
    }
}

pub fn glued_symbol(
    spec_module_path: &[Ident],
    spec_name: &Ident,
    function_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> String {
    encode(&method_symbol(
        spec_module_path,
        spec_name,
        &[],
        function_name,
        fn_type,
    ))
}

pub fn primitive_method_symbol(
    target: &ResolvedType,
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let path = ManglePath::Nested(
        Box::new(target_owner_path(target)),
        Namespace::Value,
        method_name.as_ref().to_string(),
    );
    let (params, ret) = build_signature(fn_type);
    Symbol {
        path,
        signature: Some((params, ret)),
        vendor_suffix: None,
    }
}

fn target_owner_path(target: &ResolvedType) -> ManglePath {
    match mangle_type(target) {
        MangleType::Named(path, _) => path,
        target => ManglePath::Type(Box::new(target)),
    }
}

pub fn conformance_method_symbol(
    target: &ResolvedType,
    spec_name: &Ident,
    spec_args: &[ResolvedType],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let target_path = target_owner_path(target);
    let spec = ManglePath::Nested(
        Box::new(target_path),
        Namespace::Type,
        spec_name.as_ref().to_string(),
    );
    let spec = if spec_args.is_empty() {
        spec
    } else {
        ManglePath::Generic(Box::new(spec), spec_args.iter().map(mangle_type).collect())
    };
    let path = ManglePath::Nested(
        Box::new(spec),
        Namespace::Value,
        method_name.as_ref().to_string(),
    );
    let (params, ret) = build_signature(fn_type);
    Symbol {
        path,
        signature: Some((params, ret)),
        vendor_suffix: None,
    }
}

pub fn vtable_symbol(
    concrete: &ResolvedType,
    spec_name: &Ident,
    spec_type_args: &[ResolvedType],
) -> Symbol {
    let MangleType::Named(concrete_path, _) = mangle_type(concrete) else {
        unreachable!(
            "a spec-object coercion's concrete pointee is always struct/enum/union, which always mangles to MangleType::Named"
        );
    };
    let spec_segment = ManglePath::Nested(
        Box::new(concrete_path),
        Namespace::Type,
        spec_name.as_ref().to_string(),
    );
    let with_spec = if spec_type_args.is_empty() {
        spec_segment
    } else {
        ManglePath::Generic(
            Box::new(spec_segment),
            spec_type_args.iter().map(mangle_type).collect(),
        )
    };
    let path = ManglePath::Nested(Box::new(with_spec), Namespace::Value, "vtable".to_string());
    Symbol {
        path,
        signature: None,
        vendor_suffix: None,
    }
}

pub fn encode(symbol: &Symbol) -> String {
    omega_mangle::encode(symbol)
}

pub fn data_symbol(bytes: &[u8]) -> String {
    format!("_omgdata_{:016x}", rapidhash::v3::rapidhash_v3(bytes))
}

pub fn global_symbol_string(module_path: &[Ident], name: &Ident) -> String {
    encode(&global_symbol(module_path, name))
}

pub fn extern_function_ref_symbol(extern_fn: &ExternFunctionRef) -> String {
    use omega_analyzer::checked::ExternFunctionKind;
    match (&extern_fn.mangling, &extern_fn.kind) {
        (ManglingMode::Forced(name), _) => name.clone(),
        (
            ManglingMode::Glued {
                spec_module_path,
                spec_name,
                function_name,
            },
            _,
        ) => glued_symbol(
            spec_module_path,
            spec_name,
            function_name,
            &extern_fn.fn_type,
        ),
        (ManglingMode::Disabled, ExternFunctionKind::Free(name)) => name.as_ref().to_string(),
        // Method-level disabled mangling is unreachable because analysis rejects it.
        (
            ManglingMode::Disabled,
            ExternFunctionKind::Method { .. }
            | ExternFunctionKind::Primitive { .. }
            | ExternFunctionKind::Conform { .. },
        ) => unreachable!("'@mangling(disabled)' is rejected on methods at analysis time"),
        // Extern generic instantiations are emitted locally and therefore use ordinary concrete mangling.
        (ManglingMode::Enabled, ExternFunctionKind::Free(name)) => {
            encode(&free_function_symbol(&extern_fn.module_path, name, &[], &extern_fn.fn_type))
        }
        (
            ManglingMode::Enabled,
            ExternFunctionKind::Method {
                type_name,
                method_name,
            },
        ) => encode(&method_symbol(
            &extern_fn.module_path,
            type_name,
            &[],
            method_name,
            &extern_fn.fn_type,
        )),
        (
            ManglingMode::Enabled,
            ExternFunctionKind::Primitive {
                target,
                method_name,
            },
        ) => encode(&primitive_method_symbol(target, method_name, &extern_fn.fn_type)),
        (
            ManglingMode::Enabled,
            ExternFunctionKind::Conform {
                target,
                spec_name,
                spec_args,
                method_name,
            },
        ) => encode(&conformance_method_symbol(
            target,
            spec_name,
            spec_args,
            method_name,
            &extern_fn.fn_type,
        )),
    }
}
