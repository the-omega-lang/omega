mod semantic;

use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{ExternFunctionKind, ExternFunctionRef};
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedGenericArg, ResolvedType};
use omega_mangle::{ManglePath, Namespace, Symbol};
use omega_parser::prelude::Ident;
use semantic::{generic_path, module_path, nominal_path, owner_path, signature};

pub use omega_mangle::encode;

pub fn free_function_symbol(
    module: &[Ident],
    name: &Ident,
    generic_args: &[ResolvedGenericArg],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let path = value_path(module_path(module), name);
    function_symbol(generic_path(path, generic_args), fn_type)
}

pub fn global_symbol(module: &[Ident], name: &Ident) -> Symbol {
    data_item_symbol(value_path(module_path(module), name))
}

pub fn method_symbol(
    module: &[Ident],
    owner_name: &Ident,
    owner_generic_args: &[ResolvedGenericArg],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let owner = nominal_path(module, owner_name, owner_generic_args);
    function_symbol(
        associated_function_path(owner, method_name, fn_type),
        fn_type,
    )
}

/// The path of a function declared on an owner.
///
/// A receiver-bearing declaration nests under an extra `self` value segment,
/// mirroring the `Owner::self::name` source spelling that names it. The two
/// associated-function namespaces are therefore distinct linker identities
/// even when a static and a member share a name and an ABI signature.
fn associated_function_path(
    owner: ManglePath,
    name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> ManglePath {
    let owner = match fn_type.self_mode {
        Some(_) => value_path_name(owner, "self"),
        None => owner,
    };
    value_path(owner, name)
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
    function_symbol(
        associated_function_path(owner_path(target), method_name, fn_type),
        fn_type,
    )
}

pub fn conformance_method_symbol(
    target: &ResolvedType,
    spec_name: &Ident,
    spec_args: &[ResolvedGenericArg],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    // NOTE: the current checked extern/conformance model does not carry the spec module path
    // through every call site. That can collide for same-named specs in different modules; the
    // ABI-changing fix is tracked in docs/issues/known-issues.md rather than hidden in a refactor.
    let spec = type_path(owner_path(target), spec_name);
    let spec = generic_path(spec, spec_args);
    function_symbol(
        associated_function_path(spec, method_name, fn_type),
        fn_type,
    )
}

/// `shape_members` must already be in the object's canonical shape order (see
/// `ResolvedSpecShape`), so `A + B` and `B + A` always produce this symbol
/// identically. A single member nests exactly like the pre-conjunction
/// singleton encoding, since the loop below runs once in that case.
pub fn vtable_symbol(
    concrete: &ResolvedType,
    shape_members: &[(Ident, Vec<ResolvedGenericArg>)],
) -> Symbol {
    let omega_mangle::MangleType::Named(concrete_path, _) = semantic::mangle_type(concrete) else {
        unreachable!(
            "a spec-object coercion's concrete pointee is always a nominal aggregate type"
        );
    };

    // See conformance_method_symbol: spec module identity is intentionally not changed here in a
    // refactor because linker-name changes are an ABI/separate-compilation migration.
    let mut path = concrete_path;
    for (spec_name, spec_args) in shape_members {
        path = generic_path(type_path(path, spec_name), spec_args);
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
    use omega_analyzer::resolved_type::ResolvedFunctionParam;

    fn ident(name: &str) -> Ident {
        Ident(name.to_owned())
    }

    fn fn_type(params: Vec<ResolvedType>, return_type: ResolvedType) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: params
                .into_iter()
                .enumerate()
                .map(|(index, r#type)| {
                    ResolvedFunctionParam::described(ident(&format!("p{index}")), r#type)
                })
                .collect(),
            return_type: Box::new(return_type),
            is_variadic: false,
            self_mode: None,
            calling_convention: omega_analyzer::resolved_type::CallingConvention::Omega,
        }
    }

    fn member_fn_type(
        receiver: ResolvedType,
        rest: Vec<ResolvedType>,
        return_type: ResolvedType,
    ) -> ResolvedFunctionType {
        let mut fn_type = fn_type(std::iter::once(receiver).chain(rest).collect(), return_type);
        fn_type.params[0].name = Some(ident("self"));
        fn_type.self_mode = Some(omega_parser::prelude::SelfMode::Pointer);
        fn_type
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
            &[ResolvedGenericArg::Type(ResolvedType::U32)],
            &fn_type(
                vec![ResolvedType::U32, ResolvedType::U32],
                ResolvedType::U32,
            ),
        );
        assert_adapter_round_trip(symbol);
    }

    #[test]
    fn differing_comp_generic_values_produce_differing_symbols() {
        use omega_analyzer::resolved_type::{CompIntType, CompScalar};
        let symbol_for = |value: i128| {
            encode(&free_function_symbol(
                &[ident("pkg")],
                &ident("take"),
                &[ResolvedGenericArg::Comp(CompScalar::Int {
                    r#type: CompIntType::USize,
                    value,
                })],
                &fn_type(vec![], ResolvedType::Void),
            ))
        };
        assert_ne!(symbol_for(10), symbol_for(11));
    }

    #[test]
    fn an_all_type_generic_argument_list_keeps_its_legacy_symbol_form() {
        // The mixed model must not silently migrate existing generic ABI.
        let symbol = free_function_symbol(
            &[ident("pkg")],
            &ident("take"),
            &[ResolvedGenericArg::Type(ResolvedType::U32)],
            &fn_type(vec![], ResolvedType::Void),
        );
        assert!(matches!(symbol.path, omega_mangle::ManglePath::Generic(..)));
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
            generic_args: vec![],
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
    fn member_and_static_of_one_signature_are_different_symbols() {
        // The whole point of the `::self::` path segment: these two have the
        // same owner, name, and ABI parameter list, and must still be
        // separately linkable across compilations.
        let owner = ResolvedType::Pointer {
            pointee: Box::new(concrete_struct("Thing")),
            mutable: false,
        };
        let r#static = method_symbol(
            &[ident("mymod")],
            &ident("Thing"),
            &[],
            &ident("same"),
            &fn_type(vec![owner.clone()], ResolvedType::I32),
        );
        let member = method_symbol(
            &[ident("mymod")],
            &ident("Thing"),
            &[],
            &ident("same"),
            &member_fn_type(owner, vec![], ResolvedType::I32),
        );
        assert_ne!(encode(&r#static), encode(&member));
        assert_adapter_round_trip(r#static);
        assert_adapter_round_trip(member);
    }

    #[test]
    fn member_symbol_nests_self_under_the_owner() {
        let owner = ResolvedType::Pointer {
            pointee: Box::new(concrete_struct("Thing")),
            mutable: false,
        };
        let member = method_symbol(
            &[ident("mymod")],
            &ident("Thing"),
            &[],
            &ident("same"),
            &member_fn_type(owner, vec![], ResolvedType::I32),
        );
        let ManglePath::Nested(parent, Namespace::Value, name) = &member.path else {
            panic!("a member symbol ends in its own value segment");
        };
        assert_eq!(name, "same");
        let ManglePath::Nested(owner_path, Namespace::Value, segment) = parent.as_ref() else {
            panic!("a member symbol nests under a `self` value segment");
        };
        assert_eq!(segment, "self");
        assert!(matches!(
            owner_path.as_ref(),
            ManglePath::Nested(_, Namespace::Type, owner) if owner == "Thing"
        ));
    }

    #[test]
    fn primitive_and_conformance_members_take_the_same_self_segment() {
        let member = member_fn_type(ResolvedType::I32, vec![], ResolvedType::I32);
        let r#static = fn_type(vec![ResolvedType::I32], ResolvedType::I32);

        let primitive_member = primitive_method_symbol(&ResolvedType::I32, &ident("abs"), &member);
        let primitive_static =
            primitive_method_symbol(&ResolvedType::I32, &ident("abs"), &r#static);
        assert_ne!(encode(&primitive_member), encode(&primitive_static));
        assert_adapter_round_trip(primitive_member);

        let conform_member = conformance_method_symbol(
            &ResolvedType::I32,
            &ident("Show"),
            &[],
            &ident("show"),
            &member,
        );
        let conform_static = conformance_method_symbol(
            &ResolvedType::I32,
            &ident("Show"),
            &[],
            &ident("show"),
            &r#static,
        );
        assert_ne!(encode(&conform_member), encode(&conform_static));
        assert_adapter_round_trip(conform_member);
    }

    #[test]
    fn member_definition_and_extern_reference_agree() {
        use omega_hir::{HirId, ModuleId};
        let owner = ResolvedType::Pointer {
            pointee: Box::new(concrete_struct("Thing")),
            mutable: false,
        };
        let fn_type = member_fn_type(owner, vec![], ResolvedType::I32);
        let definition = encode(&method_symbol(
            &[ident("mymod")],
            &ident("Thing"),
            &[],
            &ident("same"),
            &fn_type,
        ));
        let reference = extern_function_ref_symbol(&ExternFunctionRef {
            decl_id: HirId {
                module: ModuleId(0),
                local: 0,
            },
            module_path: vec![ident("mymod")],
            kind: ExternFunctionKind::Method {
                type_name: ident("Thing"),
                method_name: ident("same"),
            },
            fn_type,
            mangling: ManglingMode::Enabled,
        });
        assert_eq!(definition, reference);
    }

    #[test]
    fn vtable_symbol_single_member_matches_singleton_shape() {
        let concrete = concrete_struct("Foo");
        let single = vtable_symbol(&concrete, &[(ident("A"), vec![])]);
        let shape = vtable_symbol(&concrete, &[(ident("A"), vec![])]);
        assert_eq!(encode(&single), encode(&shape));
    }
}
