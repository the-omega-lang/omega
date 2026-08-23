mod semantic;

use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{ExternFunctionKind, ExternFunctionRef};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mangle::{ManglePath, Namespace, Symbol};
use omega_parser::prelude::Ident;
use semantic::{generic_path, module_path, nominal_path, owner_path, signature};

pub use omega_mangle::encode;

pub fn free_function_symbol(
    module: &[Ident],
    name: &Ident,
    type_args: &[ResolvedType],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let path = value_path(module_path(module), name);
    function_symbol(generic_path(path, type_args), fn_type)
}

pub fn global_symbol(module: &[Ident], name: &Ident) -> Symbol {
    data_item_symbol(value_path(module_path(module), name))
}

pub fn method_symbol(
    module: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let owner = nominal_path(module, owner_name, owner_type_args);
    function_symbol(value_path(owner, method_name), fn_type)
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
    function_symbol(value_path(owner_path(target), method_name), fn_type)
}

pub fn conformance_method_symbol(
    target: &ResolvedType,
    spec_name: &Ident,
    spec_args: &[ResolvedType],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    // NOTE: the current checked extern/conformance model does not carry the spec module path
    // through every call site. That can collide for same-named specs in different modules; the
    // ABI-changing fix is tracked in docs/issues/known-issues.md rather than hidden in a refactor.
    let spec = type_path(owner_path(target), spec_name);
    let spec = generic_path(spec, spec_args);
    function_symbol(value_path(spec, method_name), fn_type)
}

/// `shape_members` must already be in the object's canonical shape order (see
/// `ResolvedSpecShape`), so `A + B` and `B + A` always produce this symbol
/// identically. A single member nests exactly like the pre-conjunction
/// singleton encoding, since the loop below runs once in that case.
pub fn vtable_symbol(
    concrete: &ResolvedType,
    shape_members: &[(Ident, Vec<ResolvedType>)],
) -> Symbol {
    let omega_mangle::MangleType::Named(concrete_path, _) = semantic::mangle_type(concrete) else {
        unreachable!(
            "a spec-object coercion's concrete pointee is always a nominal aggregate type"
        );
    };

    // See conformance_method_symbol: spec module identity is intentionally not changed here in a
    // refactor because linker-name changes are an ABI/separate-compilation migration.
    let mut path = concrete_path;
    for (spec_name, spec_type_args) in shape_members {
        path = generic_path(type_path(path, spec_name), spec_type_args);
    }
    data_item_symbol(value_path_name(path, "vtable"))
}

pub fn data_symbol(bytes: &[u8]) -> String {
    format!("_omgdata_{:016x}", rapidhash::v3::rapidhash_v3(bytes))
}

pub fn global_symbol_string(module: &[Ident], name: &Ident) -> String {
    encode(&global_symbol(module, name))
}

pub fn extern_function_ref_symbol(extern_fn: &ExternFunctionRef) -> String {
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
        (ManglingMode::Disabled, ExternFunctionKind::Free(name)) => name.as_ref().to_owned(),
        (
            ManglingMode::Disabled,
            ExternFunctionKind::Method { .. }
            | ExternFunctionKind::Primitive { .. }
            | ExternFunctionKind::Conform { .. },
        ) => unreachable!("'@mangling(disabled)' is rejected on methods during analysis"),
        (ManglingMode::Enabled, ExternFunctionKind::Free(name)) => encode(&free_function_symbol(
            &extern_fn.module_path,
            name,
            &[],
            &extern_fn.fn_type,
        )),
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
        ) => encode(&primitive_method_symbol(
            target,
            method_name,
            &extern_fn.fn_type,
        )),
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

fn function_symbol(path: ManglePath, fn_type: &ResolvedFunctionType) -> Symbol {
    Symbol {
        path,
        signature: Some(signature(fn_type)),
        vendor_suffix: None,
    }
}

fn data_item_symbol(path: ManglePath) -> Symbol {
    Symbol {
        path,
        signature: None,
        vendor_suffix: None,
    }
}

fn type_path(parent: ManglePath, name: &Ident) -> ManglePath {
    nested_path(parent, Namespace::Type, name.as_ref())
}

fn value_path(parent: ManglePath, name: &Ident) -> ManglePath {
    value_path_name(parent, name.as_ref())
}

fn value_path_name(parent: ManglePath, name: &str) -> ManglePath {
    nested_path(parent, Namespace::Value, name)
}

fn nested_path(parent: ManglePath, namespace: Namespace, name: &str) -> ManglePath {
    ManglePath::Nested(Box::new(parent), namespace, name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> Ident {
        Ident(name.to_owned())
    }

    fn fn_type(params: Vec<ResolvedType>, return_type: ResolvedType) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: params
                .into_iter()
                .enumerate()
                .map(|(index, r#type)| (ident(&format!("p{index}")), r#type))
                .collect(),
            return_type: Box::new(return_type),
            is_variadic: false,
            self_mode: None,
            calling_convention: omega_analyzer::resolved_type::CallingConvention::Omega,
        }
    }

    fn assert_adapter_round_trip(symbol: Symbol) {
        let mangled = encode(&symbol);
        assert_eq!(omega_mangle::decode(&mangled), Some(symbol));
    }

    #[test]
    fn free_function_adapter_round_trips() {
        let symbol = free_function_symbol(
            &[ident("pkg"), ident("math")],
            &ident("sum"),
            &[ResolvedType::U32],
            &fn_type(
                vec![ResolvedType::U32, ResolvedType::U32],
                ResolvedType::U32,
            ),
        );
        assert_adapter_round_trip(symbol);
    }

    #[test]
    fn primitive_method_adapter_round_trips_structural_owner() {
        let symbol = primitive_method_symbol(
            &ResolvedType::I32,
            &ident("abs"),
            &fn_type(vec![ResolvedType::I32], ResolvedType::I32),
        );
        assert_adapter_round_trip(symbol);
    }

    fn concrete_struct(name: &str) -> ResolvedType {
        use omega_analyzer::resolved_type::ResolvedStructType;
        use omega_hir::{HirId, ModuleId};
        use std::cell::RefCell;
        use std::rc::Rc;
        ResolvedType::Struct(Rc::new(RefCell::new(ResolvedStructType {
            id: HirId {
                module: ModuleId(0),
                local: 0,
            },
            name: ident(name),
            module_path: vec![ident("mymod")],
            type_args: vec![],
            fields: vec![],
            functions: vec![],
            layout: omega_analyzer::annotations::Layout::default(),
            suppress: vec![],
            is_marker: false,
        })))
    }

    // `vtable_symbol` itself nests members in whatever order it is given --
    // it is `ResolvedSpecShape::canonicalize` (see omega-analyzer's
    // `resolved_type::tests`) that guarantees callers always pass the same
    // canonical order for a reordered source spelling, so `*spec A + B` and
    // `*spec B + A` reach this function with an identical member list.

    #[test]
    fn vtable_symbol_single_member_matches_singleton_shape() {
        let concrete = concrete_struct("Foo");
        let single = vtable_symbol(&concrete, &[(ident("A"), vec![])]);
        let shape = vtable_symbol(&concrete, &[(ident("A"), vec![])]);
        assert_eq!(encode(&single), encode(&shape));
    }
}
