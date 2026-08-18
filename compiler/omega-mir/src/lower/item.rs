use crate::lower::function::FunctionLowerer;
use crate::mangle;
use crate::mir::{
    MirDeclaration, MirEnumDef, MirExternDeclaration, MirFunctionDef, MirItem, MirLinkage,
    MirModule, MirStructDef, MirUnionDef,
};
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedDeclaration, CheckedEnumDef, CheckedExternDeclaration, CheckedFunctionDef, CheckedItem,
    CheckedModule, CheckedStructDef, CheckedUnionDef,
};
use omega_analyzer::resolved_type::ResolvedType;
use omega_parser::prelude::Ident;

pub(crate) fn lower_module(module: CheckedModule, path: &[Ident], entry: &[Ident]) -> MirModule {
    MirModule {
        id: module.id,
        items: module
            .items
            .into_iter()
            .map(|item| lower_item(item, path, entry))
            .collect(),
    }
}

fn lower_item(item: CheckedItem, path: &[Ident], entry: &[Ident]) -> MirItem {
    match item {
        CheckedItem::Declaration(d) => MirItem::Declaration(lower_declaration(d)),
        CheckedItem::ExternDeclaration(d) => {
            MirItem::ExternDeclaration(lower_extern_declaration(d))
        }
        CheckedItem::FunctionDefinition(f) => {
            MirItem::FunctionDefinition(lower_function_def(f, path, entry))
        }
        CheckedItem::Struct(s) => MirItem::Struct(lower_struct_def(s, path, entry)),
        CheckedItem::Enum(e) => MirItem::Enum(lower_enum_def(e, path, entry)),
        CheckedItem::Union(u) => MirItem::Union(lower_union_def(u, path, entry)),
    }
}

fn lower_declaration(decl: CheckedDeclaration) -> MirDeclaration {
    MirDeclaration {
        id: decl.id,
        span: decl.span,
        ident: decl.ident,
        r#type: decl.r#type,
        initial_value: decl.initial_value,
    }
}

fn lower_extern_declaration(decl: CheckedExternDeclaration) -> MirExternDeclaration {
    // `Disabled` externs keep their external name; glued gaps use the matching generated glue symbol.
    let symbol = match (&decl.mangling, &decl.r#type) {
        (ManglingMode::Disabled, _) => decl.ident.0.clone(),
        (
            ManglingMode::Glued {
                spec_module_path,
                spec_name,
                function_name,
            },
            ResolvedType::Function(fn_type),
        ) => mangle::glued_symbol(spec_module_path, spec_name, function_name, fn_type),
        (ManglingMode::Glued { .. }, _) => {
            unreachable!("only a gap function is Glued, and a gap function is always a function")
        }
        (ManglingMode::Enabled | ManglingMode::Forced(_), _) => {
            unreachable!("'@mangling' is rejected on 'extern' declarations at parse time")
        }
    };
    MirExternDeclaration {
        id: decl.id,
        span: decl.span,
        ident: decl.ident,
        r#type: decl.r#type,
        mangling: decl.mangling,
        symbol,
    }
}

fn lower_function_def(f: CheckedFunctionDef, path: &[Ident], entry: &[Ident]) -> MirFunctionDef {
    let (symbol, linkage) = free_function_symbol_and_linkage(&f, path, entry);
    lower_function_def_inner(f, symbol, linkage)
}

fn lower_method_def(
    f: CheckedFunctionDef,
    path: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
) -> MirFunctionDef {
    let symbol = match &f.mangling {
        // Forced method symbols survive lowering; disabled method mangling was rejected earlier.
        ManglingMode::Forced(name) => name.clone(),
        ManglingMode::Glued {
            spec_module_path,
            spec_name,
            function_name,
        } => mangle::glued_symbol(spec_module_path, spec_name, function_name, &f.fn_type()),
        ManglingMode::Disabled => {
            unreachable!("'@mangling(disabled)' is rejected on methods at analysis time")
        }
        ManglingMode::Enabled => mangle::encode(&mangle::method_symbol(
            path,
            owner_name,
            owner_type_args,
            &f.name,
            &f.fn_type(),
        )),
    };
    let linkage = if owner_type_args.is_empty() {
        MirLinkage::Export
    } else {
        MirLinkage::Weak
    };
    lower_function_def_inner(f, symbol, linkage)
}

fn lower_function_def_inner(
    f: CheckedFunctionDef,
    symbol: String,
    linkage: MirLinkage,
) -> MirFunctionDef {
    let CheckedFunctionDef {
        id,
        span,
        name,
        type_args,
        self_mode,
        is_variadic,
        params,
        return_type,
        body,
        inline,
        mangling,
        conformance_owner,
        primitive_target,
    } = f;
    // Compute mangling before moving the resolved signature fields into MIR.
    let mir_body = FunctionLowerer::lower(&params, body, &return_type, id, span);
    MirFunctionDef {
        id,
        span,
        name,
        type_args,
        self_mode,
        is_variadic,
        params,
        return_type,
        inline,
        mangling,
        conformance_owner,
        primitive_target,
        symbol,
        linkage,
        body: mir_body,
    }
}

fn free_function_symbol_and_linkage(
    f: &CheckedFunctionDef,
    path: &[Ident],
    entry: &[Ident],
) -> (String, MirLinkage) {
    // Only the designated root entry function receives the bare `main` symbol.
    let symbol = match (&f.mangling, &f.conformance_owner, &f.primitive_target) {
        (ManglingMode::Forced(name), _, _) => name.clone(),
        (
            ManglingMode::Glued {
                spec_module_path,
                spec_name,
                function_name,
            },
            _,
            _,
        ) => mangle::glued_symbol(spec_module_path, spec_name, function_name, &f.fn_type()),
        (ManglingMode::Disabled, _, _) => f.name.as_ref().to_string(),
        (ManglingMode::Enabled, _, _) if path == entry && f.name.as_ref() == "main" => {
            "main".to_string()
        }
        (ManglingMode::Enabled, Some(owner), _) => {
            mangle::encode(&mangle::conformance_method_symbol(
                &owner.target,
                &owner.spec_name,
                &owner.spec_args,
                &f.name,
                &f.fn_type(),
            ))
        }
        (ManglingMode::Enabled, None, Some(target)) => mangle::encode(
            &mangle::primitive_method_symbol(target, &f.name, &f.fn_type()),
        ),
        (ManglingMode::Enabled, None, None) => mangle::encode(&mangle::free_function_symbol(
            path,
            &f.name,
            &f.type_args,
            &f.fn_type(),
        )),
    };
    // Conformance-method identity comes from the instantiated target/spec context, not method-local generics.
    let linkage = match &f.conformance_owner {
        Some(owner) if owner.monomorphized => MirLinkage::Weak,
        _ => {
            if f.type_args.is_empty() {
                MirLinkage::Export
            } else {
                MirLinkage::Weak
            }
        }
    };
    (symbol, linkage)
}

fn lower_struct_def(s: CheckedStructDef, path: &[Ident], _entry: &[Ident]) -> MirStructDef {
    let (name, type_args, functions) = (s.name, s.type_args, s.functions);
    MirStructDef {
        id: s.id,
        span: s.span,
        name: name.clone(),
        type_args: type_args.clone(),
        fields: s.fields,
        functions: functions
            .into_iter()
            .map(|f| lower_method_def(f, path, &name, &type_args))
            .collect(),
    }
}

fn lower_union_def(u: CheckedUnionDef, path: &[Ident], _entry: &[Ident]) -> MirUnionDef {
    let (name, type_args, functions) = (u.name, u.type_args, u.functions);
    MirUnionDef {
        id: u.id,
        span: u.span,
        name: name.clone(),
        type_args: type_args.clone(),
        fields: u.fields,
        functions: functions
            .into_iter()
            .map(|f| lower_method_def(f, path, &name, &type_args))
            .collect(),
    }
}

fn lower_enum_def(e: CheckedEnumDef, path: &[Ident], _entry: &[Ident]) -> MirEnumDef {
    let (name, type_args, functions) = (e.name, e.type_args, e.functions);
    MirEnumDef {
        id: e.id,
        span: e.span,
        name: name.clone(),
        type_args: type_args.clone(),
        functions: functions
            .into_iter()
            .map(|f| lower_method_def(f, path, &name, &type_args))
            .collect(),
    }
}
