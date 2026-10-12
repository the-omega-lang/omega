mod semantic;

use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{ExternFunctionKind, ExternFunctionRef};
use omega_analyzer::resolved_type::{
    ResolvedFunctionType, ResolvedGenericArg, ResolvedSpecShape, ResolvedType,
};
use omega_analyzer::template::TemplateDescriptor;
use omega_hir::ModulePath;
use omega_mangle::{ManglePath, Namespace, Symbol};
use omega_parser::prelude::Ident;
use semantic::{generic_path, mangle_template, module_path, nominal_path, owner_path, signature};

pub use omega_mangle::encode;

pub fn free_function_symbol(
    module: &ModulePath,
    name: &Ident,
    template: Option<&TemplateDescriptor>,
    generic_args: &[ResolvedGenericArg],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let path = value_path(module_path(module), name);
    function_symbol(
        generic_path(template_path(path, template), generic_args),
        fn_type,
    )
}

/// The declaration identity a generic function's symbol carries, between the
/// path that names it and its concrete arguments.
///
/// Two declarations of one name can be selected at the same arguments with
/// the same resulting signature, so without this they would encode to one
/// weak symbol and separately compiled callers would silently share a body.
/// A declaration with no generic parameters of its own has nothing to
/// disambiguate and keeps its existing symbol.
fn template_path(path: ManglePath, template: Option<&TemplateDescriptor>) -> ManglePath {
    match template {
        Some(template) => ManglePath::Template(Box::new(path), Box::new(mangle_template(template))),
        None => path,
    }
}

pub fn global_symbol(module: &ModulePath, name: &Ident) -> Symbol {
    data_item_symbol(value_path(module_path(module), name))
}

/// A function declared on a named owner. `generic_args` are the *method's*
/// own, which an instantiated generic method needs in its path for the same
/// reason a generic free function does: two instantiations of one
/// declaration are two symbols.
#[allow(clippy::too_many_arguments)]
pub fn method_symbol(
    module: &ModulePath,
    owner_name: &Ident,
    owner_generic_args: &[ResolvedGenericArg],
    method_name: &Ident,
    template: Option<&TemplateDescriptor>,
    generic_args: &[ResolvedGenericArg],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let owner = nominal_path(module, owner_name, owner_generic_args);
    function_symbol(
        generic_path(
            template_path(
                associated_function_path(owner, method_name, fn_type),
                template,
            ),
            generic_args,
        ),
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
    spec_module_path: &ModulePath,
    spec_name: &Ident,
    function_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> String {
    encode(&method_symbol(
        spec_module_path,
        spec_name,
        &[],
        function_name,
        None,
        &[],
        fn_type,
    ))
}

pub fn primitive_method_symbol(
    target: &ResolvedType,
    method_name: &Ident,
    template: Option<&TemplateDescriptor>,
    generic_args: &[ResolvedGenericArg],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let path = associated_function_path(owner_path(target), method_name, fn_type);
    function_symbol(
        generic_path(template_path(path, template), generic_args),
        fn_type,
    )
}

/// A method of the conformance of `target` to one spec, named under
/// `<target as spec>` so two same-named specs from different modules never
/// share a symbol.
#[allow(clippy::too_many_arguments)]
pub fn conformance_method_symbol(
    target: &ResolvedType,
    spec_module_path: &ModulePath,
    spec_name: &Ident,
    spec_args: &[ResolvedGenericArg],
    method_name: &Ident,
    template: Option<&TemplateDescriptor>,
    generic_args: &[ResolvedGenericArg],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let conformance = conformance_path(
        target,
        vec![nominal_path(spec_module_path, spec_name, spec_args)],
    );
    let path = associated_function_path(conformance, method_name, fn_type);
    function_symbol(
        generic_path(template_path(path, template), generic_args),
        fn_type,
    )
}

/// `shape.members` is already in canonical shape order, so `A + B` and
/// `B + A` produce one symbol; nothing here re-sorts.
pub fn vtable_symbol(concrete: &ResolvedType, shape: &ResolvedSpecShape) -> Symbol {
    let specs = shape
        .members
        .iter()
        .map(|member| {
            let spec = member.spec.borrow();
            nominal_path(&spec.module_path, &spec.name, &member.spec_args)
        })
        .collect();
    data_item_symbol(value_path_name(conformance_path(concrete, specs), "vtable"))
}

fn conformance_path(target: &ResolvedType, specs: Vec<ManglePath>) -> ManglePath {
    ManglePath::Conformance(Box::new(semantic::mangle_type(target)), specs)
}

/// The `core::reflection::TypeInfo` table of `described`, which must not be
/// a refined enum. The name depends only on the type, so every compilation
/// unit that emits the table agrees on it and the copies merge.
pub fn typeinfo_symbol(described: &ResolvedType) -> Symbol {
    data_item_symbol(value_path_name(owner_path(described), "typeinfo"))
}

/// The value image of `enum_type` holding variant `variant`; the same
/// naming rule as `typeinfo_symbol`.
pub fn variant_prototype_symbol(enum_type: &ResolvedType, variant: usize) -> Symbol {
    data_item_symbol(value_path_name(
        value_path_name(owner_path(enum_type), "typeinfo"),
        &format!("prototype{variant}"),
    ))
}

pub fn data_symbol(bytes: &[u8]) -> String {
    format!("_omgdata_{:016x}", rapidhash::v3::rapidhash_v3(bytes))
}

pub fn global_symbol_string(module: &ModulePath, name: &Ident) -> String {
    encode(&global_symbol(module, name))
}

/// The linker symbol of module-level data storage, whether an Omega global
/// defines it or a `foreign` binding names it. Lowering a definition and
/// declaring another package's global both use this, so the two always agree.
pub fn data_global_symbol(mangling: &ManglingMode, module: &ModulePath, name: &Ident) -> String {
    match mangling {
        ManglingMode::Enabled => global_symbol_string(module, name),
        ManglingMode::Disabled => name.as_ref().to_owned(),
        ManglingMode::Forced(symbol) => symbol.clone(),
        ManglingMode::Glued { .. } => {
            unreachable!("only function-valued gap declarations use glued mangling")
        }
    }
}

pub fn extern_function_ref_symbol(extern_fn: &ExternFunctionRef) -> String {
    match (&extern_fn.symbol.mangling, &extern_fn.kind) {
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
        ) => unreachable!("'@symbol(mangle = disabled)' is rejected on methods during analysis"),
        // An extern reference is a *nongeneric* external declaration (see
        // `ExternFunctionKind`); a concrete instantiation of an imported
        // generic template is emitted locally from its own checked body,
        // where the declaration's descriptor is available.
        (ManglingMode::Enabled, ExternFunctionKind::Free(name)) => encode(&free_function_symbol(
            &extern_fn.module_path,
            name,
            None,
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
            None,
            &[],
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
            None,
            &[],
            &extern_fn.fn_type,
        )),
        (
            ManglingMode::Enabled,
            ExternFunctionKind::Conform {
                target,
                spec_module_path,
                spec_name,
                spec_args,
                method_name,
            },
        ) => encode(&conformance_method_symbol(
            target,
            spec_module_path,
            spec_name,
            spec_args,
            method_name,
            None,
            &[],
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
    use omega_analyzer::annotations::SymbolPolicy;
    use omega_analyzer::resolved_type::{ResolvedFunctionParam, ResolvedSpecApplication};
    use omega_analyzer::template::{TemplateBound, TemplateParam, TemplateType};

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
            &ModulePath::new([ident("pkg"), ident("math")]),
            &ident("sum"),
            None,
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
                &ModulePath::new([ident("pkg")]),
                &ident("take"),
                None,
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
            &ModulePath::new([ident("pkg")]),
            &ident("take"),
            None,
            &[ResolvedGenericArg::Type(ResolvedType::U32)],
            &fn_type(vec![], ResolvedType::Void),
        );
        assert!(matches!(symbol.path, omega_mangle::ManglePath::Generic(..)));
        assert_adapter_round_trip(symbol);
    }

    #[test]
    fn a_method_instantiation_carries_its_own_generic_arguments() {
        // Two instantiations of one declaration are two symbols, exactly as
        // for a generic free function.
        let owner = ResolvedType::Pointer {
            pointee: Box::new(concrete_struct("Thing")),
            mutable: false,
        };
        let symbol_for = |argument: ResolvedType| {
            method_symbol(
                &ModulePath::new([ident("mymod")]),
                &ident("Thing"),
                &[],
                &ident("echo"),
                None,
                &[ResolvedGenericArg::Type(argument.clone())],
                &member_fn_type(owner.clone(), vec![argument.clone()], argument),
            )
        };
        let u32_instance = symbol_for(ResolvedType::U32);
        let u64_instance = symbol_for(ResolvedType::U64);
        assert_ne!(encode(&u32_instance), encode(&u64_instance));
        assert_adapter_round_trip(u32_instance);
    }

    #[test]
    fn a_method_without_generic_arguments_keeps_its_symbol() {
        // Method generic arguments must not migrate the symbol of every
        // ordinary method that has none.
        let owner = ResolvedType::Pointer {
            pointee: Box::new(concrete_struct("Thing")),
            mutable: false,
        };
        let fn_type = member_fn_type(owner, vec![], ResolvedType::I32);
        let symbol = method_symbol(
            &ModulePath::new([ident("mymod")]),
            &ident("Thing"),
            &[],
            &ident("same"),
            None,
            &[],
            &fn_type,
        );
        assert!(matches!(symbol.path, omega_mangle::ManglePath::Nested(..)));
    }

    #[test]
    fn primitive_method_adapter_round_trips_structural_owner() {
        let symbol = primitive_method_symbol(
            &ResolvedType::I32,
            &ident("abs"),
            None,
            &[],
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
            module_path: omega_hir::ModulePath::from(vec![ident("mymod")]),
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
            &ModulePath::new([ident("mymod")]),
            &ident("Thing"),
            &[],
            &ident("same"),
            None,
            &[],
            &fn_type(vec![owner.clone()], ResolvedType::I32),
        );
        let member = method_symbol(
            &ModulePath::new([ident("mymod")]),
            &ident("Thing"),
            &[],
            &ident("same"),
            None,
            &[],
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
            &ModulePath::new([ident("mymod")]),
            &ident("Thing"),
            &[],
            &ident("same"),
            None,
            &[],
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

        let primitive_member =
            primitive_method_symbol(&ResolvedType::I32, &ident("abs"), None, &[], &member);
        let primitive_static =
            primitive_method_symbol(&ResolvedType::I32, &ident("abs"), None, &[], &r#static);
        assert_ne!(encode(&primitive_member), encode(&primitive_static));
        assert_adapter_round_trip(primitive_member);

        let conform_member = conformance_method_symbol(
            &ResolvedType::I32,
            &ModulePath::new([ident("pkg")]),
            &ident("Show"),
            &[],
            &ident("show"),
            None,
            &[],
            &member,
        );
        let conform_static = conformance_method_symbol(
            &ResolvedType::I32,
            &ModulePath::new([ident("pkg")]),
            &ident("Show"),
            &[],
            &ident("show"),
            None,
            &[],
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
            &ModulePath::new([ident("mymod")]),
            &ident("Thing"),
            &[],
            &ident("same"),
            None,
            &[],
            &fn_type,
        ));
        let reference = extern_function_ref_symbol(&ExternFunctionRef {
            decl_id: HirId {
                module: ModuleId(0),
                local: 0,
            },
            module_path: omega_hir::ModulePath::from(vec![ident("mymod")]),
            kind: ExternFunctionKind::Method {
                type_name: ident("Thing"),
                method_name: ident("same"),
            },
            fn_type,
            symbol: SymbolPolicy::ordinary(),
        });
        assert_eq!(definition, reference);
    }

    fn template(bounds: Vec<TemplateBound>) -> TemplateDescriptor {
        TemplateDescriptor {
            generics: vec![TemplateParam::Type(bounds)],
            params: vec![TemplateType::Param(0)],
            return_type: TemplateType::Fixed(ResolvedType::I32),
            is_variadic: false,
            convention: omega_analyzer::resolved_type::CallingConvention::Omega,
        }
    }

    fn bound(name: &str, args: Vec<omega_analyzer::template::TemplateArg>) -> TemplateBound {
        TemplateBound {
            module_path: omega_hir::ModulePath::from(vec![ident("pkg")]),
            name: ident(name),
            args,
        }
    }

    #[test]
    fn a_template_qualified_function_round_trips_and_precedes_its_arguments() {
        let symbol = free_function_symbol(
            &ModulePath::new([ident("pkg")]),
            &ident("pick"),
            Some(&template(vec![bound("A", vec![])])),
            &[ResolvedGenericArg::Type(ResolvedType::U32)],
            &fn_type(vec![ResolvedType::U32], ResolvedType::I32),
        );
        // The concrete application is outermost, so the descriptor qualifies
        // the declaration rather than the instantiation.
        let ManglePath::Generic(inner, _) = &symbol.path else {
            panic!("an instantiation's path ends in its concrete arguments");
        };
        assert!(matches!(inner.as_ref(), ManglePath::Template(..)));
        assert_adapter_round_trip(symbol);
    }

    #[test]
    fn two_declarations_at_one_instantiation_produce_two_symbols() {
        let symbol_for = |descriptor: TemplateDescriptor| {
            encode(&free_function_symbol(
                &ModulePath::new([ident("pkg")]),
                &ident("pick"),
                Some(&descriptor),
                &[ResolvedGenericArg::Type(ResolvedType::U32)],
                &fn_type(vec![ResolvedType::U32], ResolvedType::I32),
            ))
        };
        let a = symbol_for(template(vec![bound("A", vec![])]));
        let b = symbol_for(template(vec![bound("B", vec![])]));
        assert_ne!(a, b);
        // A declaration with none of its own keeps the symbol it already had.
        assert_ne!(
            a,
            encode(&free_function_symbol(
                &ModulePath::new([ident("pkg")]),
                &ident("pick"),
                None,
                &[ResolvedGenericArg::Type(ResolvedType::U32)],
                &fn_type(vec![ResolvedType::U32], ResolvedType::I32),
            ))
        );
    }

    #[test]
    fn a_symbolic_bound_argument_is_not_the_type_it_is_instantiated_with() {
        use omega_analyzer::template::TemplateArg;
        let symbol_for = |argument: TemplateArg| {
            encode(&free_function_symbol(
                &ModulePath::new([ident("pkg")]),
                &ident("pick"),
                Some(&template(vec![bound("Holds", vec![argument])])),
                &[ResolvedGenericArg::Type(ResolvedType::U32)],
                &fn_type(vec![ResolvedType::U32], ResolvedType::I32),
            ))
        };
        assert_ne!(
            symbol_for(TemplateArg::Type(TemplateType::Param(0))),
            symbol_for(TemplateArg::Type(TemplateType::Fixed(ResolvedType::U32))),
        );
    }

    fn spec_application(id: u32, module: &str, name: &str) -> ResolvedSpecApplication {
        use omega_analyzer::resolved_type::ResolvedSpecType;
        use omega_hir::{HirId, ModuleId};
        use omega_parser::prelude::Visibility;
        use std::cell::RefCell;
        use std::rc::Rc;
        let spec = ResolvedSpecType {
            id: HirId {
                module: ModuleId(0),
                local: id,
            },
            name: ident(name),
            visibility: Visibility::Exposed,
            generics: vec![],
            module_path: omega_hir::ModulePath::from(vec![ident("pkg"), ident(module)]),
            generic_args: vec![],
            is_object_safe: true,
            functions: vec![],
            suppress: vec![],
        };
        ResolvedSpecApplication::new(Rc::new(RefCell::new(spec)), vec![])
    }

    fn shape(members: Vec<ResolvedSpecApplication>) -> ResolvedSpecShape {
        ResolvedSpecShape::canonicalize(members)
    }

    #[test]
    fn same_named_specs_in_two_modules_give_two_conformance_methods() {
        let target = concrete_struct("Thing");
        let member = member_fn_type(target.clone(), vec![], ResolvedType::I32);
        let symbol_in = |module: &str| {
            conformance_method_symbol(
                &target,
                &ModulePath::new([ident("pkg"), ident(module)]),
                &ident("P"),
                &[],
                &ident("f"),
                None,
                &[],
                &member,
            )
        };
        assert_ne!(encode(&symbol_in("a")), encode(&symbol_in("b")));
        assert_adapter_round_trip(symbol_in("a"));
    }

    #[test]
    fn shapes_differing_only_in_a_member_module_give_two_vtables() {
        let concrete = concrete_struct("Thing");
        let a = vtable_symbol(&concrete, &shape(vec![spec_application(1, "a", "P")]));
        let b = vtable_symbol(&concrete, &shape(vec![spec_application(2, "b", "P")]));
        assert_ne!(encode(&a), encode(&b));
        assert_adapter_round_trip(a);
    }

    #[test]
    fn a_conjunction_vtable_names_every_member() {
        let concrete = concrete_struct("Thing");
        let both = vtable_symbol(
            &concrete,
            &shape(vec![
                spec_application(1, "a", "P"),
                spec_application(2, "b", "P"),
            ]),
        );
        assert_eq!(
            omega_mangle::demangle(&encode(&both)).unwrap(),
            "<mymod::Thing as pkg::a::P + pkg::b::P>::vtable"
        );
        assert_adapter_round_trip(both);
    }
}
