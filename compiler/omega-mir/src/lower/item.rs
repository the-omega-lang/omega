use crate::body::{MirAsmOperand, MirAsmOperandKind, MirInlineAsm};
use crate::lower::function::FunctionLowerer;
use crate::mangle;
use crate::mir::{
    MirDeclaration, MirEnumDef, MirExternDeclaration, MirFunctionBody, MirFunctionDef, MirItem,
    MirLinkage, MirModule, MirStructDef, MirUnionDef,
};
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedAsmDescriptorKind, CheckedBlock, CheckedDeclaration, CheckedEnumDef,
    CheckedExternDeclaration, CheckedFunctionDef, CheckedItem, CheckedModule, CheckedStmt,
    CheckedStructDef, CheckedUnionDef,
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
        CheckedItem::Declaration(declaration) => {
            MirItem::Declaration(lower_declaration(declaration))
        }
        CheckedItem::ExternDeclaration(declaration) => {
            MirItem::ExternDeclaration(lower_extern_declaration(declaration))
        }
        CheckedItem::FunctionDefinition(function) => {
            MirItem::FunctionDefinition(lower_free_function(function, path, entry))
        }
        CheckedItem::Struct(definition) => MirItem::Struct(lower_struct_def(definition, path)),
        CheckedItem::Enum(definition) => MirItem::Enum(lower_enum_def(definition, path)),
        CheckedItem::Union(definition) => MirItem::Union(lower_union_def(definition, path)),
    }
}

fn lower_declaration(declaration: CheckedDeclaration) -> MirDeclaration {
    MirDeclaration {
        id: declaration.id,
        span: declaration.span,
        ident: declaration.ident,
        r#type: declaration.r#type,
        initial_value: declaration.initial_value,
    }
}

fn lower_extern_declaration(declaration: CheckedExternDeclaration) -> MirExternDeclaration {
    let symbol = extern_declaration_symbol(&declaration);
    MirExternDeclaration {
        id: declaration.id,
        span: declaration.span,
        ident: declaration.ident,
        r#type: declaration.r#type,
        mangling: declaration.mangling,
        symbol,
    }
}

fn extern_declaration_symbol(declaration: &CheckedExternDeclaration) -> String {
    match (&declaration.mangling, &declaration.r#type) {
        (ManglingMode::Disabled, _) => declaration.ident.as_ref().to_owned(),
        (
            ManglingMode::Glued {
                spec_module_path,
                spec_name,
                function_name,
            },
            ResolvedType::Function(fn_type),
        ) => mangle::glued_symbol(spec_module_path, spec_name, function_name, fn_type),
        (ManglingMode::Glued { .. }, _) => {
            unreachable!("only function-valued gap declarations use glued mangling")
        }
        (ManglingMode::Enabled | ManglingMode::Forced(_), _) => {
            unreachable!("'@mangling' is rejected on extern declarations during parsing")
        }
    }
}

fn lower_free_function(
    function: CheckedFunctionDef,
    path: &[Ident],
    entry: &[Ident],
) -> MirFunctionDef {
    let symbol = free_function_symbol(&function, path, entry);
    let linkage = function_linkage(&function);
    lower_function(function, symbol, linkage)
}

fn lower_method(
    function: CheckedFunctionDef,
    path: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
) -> MirFunctionDef {
    let symbol = method_symbol(&function, path, owner_name, owner_type_args);
    let linkage = if owner_type_args.is_empty() {
        MirLinkage::Export
    } else {
        MirLinkage::Weak
    };
    lower_function(function, symbol, linkage)
}

fn lower_function(
    function: CheckedFunctionDef,
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
        naked,
    } = function;
    let body = if naked {
        MirFunctionBody::Naked(lower_naked_body(body))
    } else {
        MirFunctionBody::Normal(FunctionLowerer::lower(&params, body, &return_type, id, span))
    };

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
        body,
    }
}

/// Converts the sole checked `asm` of a validated `@naked` body directly to
/// `MirInlineAsm`, bypassing `FunctionLowerer` entirely: naked functions get
/// no locals, no parameter homes, and no CFG. `reg` descriptors cannot reach
/// this point -- the analyzer rejects them inside a naked function's `asm`.
fn lower_naked_body(body: CheckedBlock) -> MirInlineAsm {
    let mut stmts = body.stmts.into_iter();
    let (Some(CheckedStmt::InlineAsm(asm)), None) = (stmts.next(), stmts.next()) else {
        unreachable!(
            "analyzer guarantees a naked function's body is exactly one InlineAsm statement"
        )
    };

    let mut operands = Vec::with_capacity(asm.descriptors.len());
    let mut clobbers = Vec::new();
    for descriptor in asm.descriptors {
        match descriptor.kind {
            CheckedAsmDescriptorKind::Reg { .. } => {
                unreachable!("analyzer rejects 'reg' descriptors inside a naked function's asm")
            }
            CheckedAsmDescriptorKind::Const { text } => {
                operands.push(MirAsmOperand {
                    binding_name: descriptor.binding_name,
                    kind: MirAsmOperandKind::Const { text },
                });
            }
            CheckedAsmDescriptorKind::Clobber { register } => clobbers.push(register),
        }
    }

    MirInlineAsm {
        operands,
        clobbers,
        template: asm.body,
        template_span: asm.body_span,
    }
}

fn free_function_symbol(function: &CheckedFunctionDef, path: &[Ident], entry: &[Ident]) -> String {
    match (
        &function.mangling,
        &function.conformance_owner,
        &function.primitive_target,
    ) {
        (ManglingMode::Forced(name), _, _) => name.clone(),
        (
            ManglingMode::Glued {
                spec_module_path,
                spec_name,
                function_name,
            },
            _,
            _,
        ) => mangle::glued_symbol(
            spec_module_path,
            spec_name,
            function_name,
            &function.fn_type(),
        ),
        (ManglingMode::Disabled, _, _) => function.name.as_ref().to_owned(),
        (ManglingMode::Enabled, _, _) if is_root_main(function, path, entry) => {
            "_omg_main".to_owned()
        }
        (ManglingMode::Enabled, Some(owner), _) => {
            mangle::encode(&mangle::conformance_method_symbol(
                &owner.target,
                &owner.spec_name,
                &owner.spec_args,
                &function.name,
                &function.fn_type(),
            ))
        }
        (ManglingMode::Enabled, None, Some(target)) => mangle::encode(
            &mangle::primitive_method_symbol(target, &function.name, &function.fn_type()),
        ),
        (ManglingMode::Enabled, None, None) => mangle::encode(&mangle::free_function_symbol(
            path,
            &function.name,
            &function.type_args,
            &function.fn_type(),
        )),
    }
}

fn method_symbol(
    function: &CheckedFunctionDef,
    path: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
) -> String {
    match &function.mangling {
        ManglingMode::Forced(name) => name.clone(),
        ManglingMode::Glued {
            spec_module_path,
            spec_name,
            function_name,
        } => mangle::glued_symbol(
            spec_module_path,
            spec_name,
            function_name,
            &function.fn_type(),
        ),
        ManglingMode::Disabled => {
            unreachable!("'@mangling(disabled)' is rejected on methods during analysis")
        }
        ManglingMode::Enabled => mangle::encode(&mangle::method_symbol(
            path,
            owner_name,
            owner_type_args,
            &function.name,
            &function.fn_type(),
        )),
    }
}

fn function_linkage(function: &CheckedFunctionDef) -> MirLinkage {
    if function
        .conformance_owner
        .as_ref()
        .is_some_and(|owner| owner.monomorphized)
        || !function.type_args.is_empty()
    {
        MirLinkage::Weak
    } else {
        MirLinkage::Export
    }
}

fn is_root_main(function: &CheckedFunctionDef, path: &[Ident], entry: &[Ident]) -> bool {
    path == entry && function.name.as_ref() == "main"
}

fn lower_methods(
    functions: Vec<CheckedFunctionDef>,
    path: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
) -> Vec<MirFunctionDef> {
    functions
        .into_iter()
        .map(|function| lower_method(function, path, owner_name, owner_type_args))
        .collect()
}

fn lower_struct_def(definition: CheckedStructDef, path: &[Ident]) -> MirStructDef {
    let CheckedStructDef {
        id,
        span,
        name,
        type_args,
        fields,
        functions,
    } = definition;
    let functions = lower_methods(functions, path, &name, &type_args);

    MirStructDef {
        id,
        span,
        name,
        type_args,
        fields,
        functions,
    }
}

fn lower_union_def(definition: CheckedUnionDef, path: &[Ident]) -> MirUnionDef {
    let CheckedUnionDef {
        id,
        span,
        name,
        type_args,
        fields,
        functions,
    } = definition;
    let functions = lower_methods(functions, path, &name, &type_args);

    MirUnionDef {
        id,
        span,
        name,
        type_args,
        fields,
        functions,
    }
}

fn lower_enum_def(definition: CheckedEnumDef, path: &[Ident]) -> MirEnumDef {
    let CheckedEnumDef {
        id,
        span,
        name,
        type_args,
        functions,
    } = definition;
    let functions = lower_methods(functions, path, &name, &type_args);

    MirEnumDef {
        id,
        span,
        name,
        type_args,
        functions,
    }
}
