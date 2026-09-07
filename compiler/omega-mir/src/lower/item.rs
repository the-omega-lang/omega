use crate::body::{MirAsmOperand, MirAsmOperandKind, MirInlineAsm};
use crate::lower::function::FunctionLowerer;
use crate::mangle;
use crate::mir::{
    MirDeclaration, MirEnumDef, MirForeignBinding, MirForeignFunctionDef, MirFunctionBody,
    MirFunctionDef, MirItem, MirLinkage, MirModule, MirStructDef, MirUnionDef,
};
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{
    CheckedAsmDescriptorKind, CheckedBlock, CheckedDeclaration, CheckedEnumDef,
    CheckedForeignBinding, CheckedForeignFunctionDef, CheckedFunctionDef, CheckedItem,
    CheckedModule, CheckedStmt, CheckedStructDef, CheckedUnionDef,
};
use omega_analyzer::resolved_type::{ResolvedGenericArg, ResolvedType};
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
            MirItem::Declaration(lower_declaration(declaration, path))
        }
        CheckedItem::ForeignBinding(binding) => {
            MirItem::ForeignBinding(lower_foreign_binding(binding, path))
        }
        CheckedItem::ForeignFunction(function) => {
            MirItem::ForeignFunction(lower_foreign_function(function, path))
        }
        CheckedItem::FunctionDefinition(function) => {
            MirItem::FunctionDefinition(lower_free_function(function, path, entry))
        }
        CheckedItem::Struct(definition) => MirItem::Struct(lower_struct_def(definition, path)),
        CheckedItem::Enum(definition) => MirItem::Enum(lower_enum_def(definition, path)),
        CheckedItem::Union(definition) => MirItem::Union(lower_union_def(definition, path)),
    }
}

fn lower_declaration(declaration: CheckedDeclaration, path: &[Ident]) -> MirDeclaration {
    let symbol = global_symbol(&declaration, path);
    MirDeclaration {
        id: declaration.id,
        span: declaration.span,
        ident: declaration.ident,
        r#type: declaration.r#type,
        initial_value: declaration.initial_value,
        symbol,
    }
}

/// A definition owns storage, so its symbol is always a data symbol: a global
/// whose type happens to be a function type still names its own storage, not
/// the function it holds.
fn global_symbol(declaration: &CheckedDeclaration, path: &[Ident]) -> String {
    match &declaration.mangling {
        ManglingMode::Enabled => mangle::global_symbol_string(path, &declaration.ident),
        ManglingMode::Disabled => declaration.ident.as_ref().to_owned(),
        ManglingMode::Forced(name) => name.clone(),
        ManglingMode::Glued { .. } => {
            unreachable!("only a gap declaration uses glued mangling")
        }
    }
}

fn lower_foreign_binding(declaration: CheckedForeignBinding, path: &[Ident]) -> MirForeignBinding {
    let symbol = foreign_binding_symbol(&declaration, path);
    MirForeignBinding {
        id: declaration.id,
        span: declaration.span,
        ident: declaration.ident,
        r#type: declaration.r#type,
        mangling: declaration.mangling,
        symbol,
    }
}

/// Mirrors `free_function_symbol`'s enabled/forced/disabled policy (see
/// `docs/language/foreign-function-interface.md`): disabled uses the source
/// name verbatim, forced/glued use their exact symbol, and enabled builds an
/// ordinary Omega function/global symbol from module identity and type.
fn foreign_binding_symbol(declaration: &CheckedForeignBinding, path: &[Ident]) -> String {
    match (&declaration.mangling, &declaration.r#type) {
        (ManglingMode::Disabled, _) => declaration.ident.as_ref().to_owned(),
        (ManglingMode::Forced(name), _) => name.clone(),
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
        (ManglingMode::Enabled, ResolvedType::Function(fn_type)) => mangle::encode(
            &mangle::free_function_symbol(path, &declaration.ident, &[], fn_type),
        ),
        (ManglingMode::Enabled, _) => mangle::global_symbol_string(path, &declaration.ident),
    }
}

fn lower_foreign_function(
    function: CheckedForeignFunctionDef,
    path: &[Ident],
) -> MirForeignFunctionDef {
    let symbol = foreign_function_symbol(&function, path);
    let CheckedForeignFunctionDef {
        id,
        span,
        name,
        calling_convention,
        is_variadic,
        params,
        return_type,
        body,
        mangling,
    } = function;
    let body = body.map(|body| {
        MirFunctionBody::Normal(FunctionLowerer::lower(
            &params,
            body,
            &return_type,
            id,
            span,
        ))
    });
    MirForeignFunctionDef {
        id,
        span,
        name,
        calling_convention,
        is_variadic,
        params,
        return_type,
        mangling,
        symbol,
        linkage: MirLinkage::Export,
        body,
    }
}

fn foreign_function_symbol(function: &CheckedForeignFunctionDef, path: &[Ident]) -> String {
    match &function.mangling {
        ManglingMode::Disabled => function.name.as_ref().to_owned(),
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
        ManglingMode::Enabled => mangle::encode(&mangle::free_function_symbol(
            path,
            &function.name,
            &[],
            &function.fn_type(),
        )),
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
    owner_generic_args: &[ResolvedGenericArg],
) -> MirFunctionDef {
    let symbol = method_symbol(&function, path, owner_name, owner_generic_args);
    let linkage = if owner_generic_args.is_empty() {
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
        generic_args,
        self_mode,
        is_variadic,
        params,
        return_type,
        body,
        inline,
        mangling,
        conformance_owner,
        primitive_target,
        method_owner: _,
        naked,
    } = function;
    let body = if naked {
        MirFunctionBody::Naked(lower_naked_body(body))
    } else {
        MirFunctionBody::Normal(FunctionLowerer::lower(
            &params,
            body,
            &return_type,
            id,
            span,
        ))
    };

    MirFunctionDef {
        id,
        span,
        name,
        generic_args,
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
            CheckedAsmDescriptorKind::Comp { text } => {
                operands.push(MirAsmOperand {
                    binding_name: descriptor.binding_name,
                    kind: MirAsmOperandKind::Comp { text },
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

/// The symbol of a checked function emitted on its own, which covers three
/// owned forms as well as an ordinary free function: conformance methods,
/// primitive methods, and instantiated generic methods are all emitted
/// outside their owner's definition and name their owner here.
fn free_function_symbol(function: &CheckedFunctionDef, path: &[Ident], entry: &[Ident]) -> String {
    if let ManglingMode::Enabled = &function.mangling
        && let Some(owner) = &function.method_owner
    {
        return mangle::encode(&mangle::method_symbol(
            &owner.module_path,
            &owner.name,
            &owner.generic_args,
            &function.name,
            &function.generic_args,
            &function.fn_type(),
        ));
    }
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
            &function.generic_args,
            &function.fn_type(),
        )),
    }
}

fn method_symbol(
    function: &CheckedFunctionDef,
    path: &[Ident],
    owner_name: &Ident,
    owner_generic_args: &[ResolvedGenericArg],
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
            owner_generic_args,
            &function.name,
            &function.generic_args,
            &function.fn_type(),
        )),
    }
}

fn function_linkage(function: &CheckedFunctionDef) -> MirLinkage {
    if function
        .conformance_owner
        .as_ref()
        .is_some_and(|owner| owner.monomorphized)
        || !function.generic_args.is_empty()
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
    owner_generic_args: &[ResolvedGenericArg],
) -> Vec<MirFunctionDef> {
    functions
        .into_iter()
        .map(|function| lower_method(function, path, owner_name, owner_generic_args))
        .collect()
}

fn lower_struct_def(definition: CheckedStructDef, path: &[Ident]) -> MirStructDef {
    let CheckedStructDef {
        id,
        span,
        name,
        generic_args,
        fields,
        functions,
    } = definition;
    let functions = lower_methods(functions, path, &name, &generic_args);

    MirStructDef {
        id,
        span,
        name,
        generic_args,
        fields,
        functions,
    }
}

fn lower_union_def(definition: CheckedUnionDef, path: &[Ident]) -> MirUnionDef {
    let CheckedUnionDef {
        id,
        span,
        name,
        generic_args,
        fields,
        functions,
    } = definition;
    let functions = lower_methods(functions, path, &name, &generic_args);

    MirUnionDef {
        id,
        span,
        name,
        generic_args,
        fields,
        functions,
    }
}

fn lower_enum_def(definition: CheckedEnumDef, path: &[Ident]) -> MirEnumDef {
    let CheckedEnumDef {
        id,
        span,
        name,
        generic_args,
        functions,
    } = definition;
    let functions = lower_methods(functions, path, &name, &generic_args);

    MirEnumDef {
        id,
        span,
        name,
        generic_args,
        functions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_hir::{HirId, ModuleId};
    use omega_parser::prelude::Span;

    fn function_type() -> ResolvedType {
        ResolvedType::Function(omega_analyzer::resolved_type::ResolvedFunctionType {
            params: Vec::new(),
            return_type: Box::new(ResolvedType::Void),
            is_variadic: false,
            self_mode: None,
            calling_convention: omega_analyzer::resolved_type::CallingConvention::Omega,
        })
    }

    fn declaration(name: &str) -> CheckedDeclaration {
        CheckedDeclaration {
            id: HirId {
                module: ModuleId(0),
                local: 0,
            },
            span: Span::default(),
            ident: Ident(name.to_string()),
            r#type: ResolvedType::I32,
            mutable: true,
            initial_value: None,
            mangling: ManglingMode::Enabled,
        }
    }

    fn path(segments: &[&str]) -> Vec<Ident> {
        segments.iter().map(|s| Ident(s.to_string())).collect()
    }

    /// A global's linker identity is settled here, from its declaring module
    /// alone. Nothing downstream -- including which source file's object it is
    /// emitted into -- can change it.
    #[test]
    fn a_global_symbol_is_decided_by_its_declaring_module_before_emission() {
        let counter = path(&["pkg", "counter"]);
        let other = path(&["pkg", "other"]);

        assert_eq!(
            lower_declaration(declaration("TOTAL"), &counter).symbol,
            mangle::global_symbol_string(&counter, &Ident("TOTAL".to_string()))
        );
        assert_ne!(
            lower_declaration(declaration("TOTAL"), &counter).symbol,
            lower_declaration(declaration("TOTAL"), &other).symbol,
            "the declaring module distinguishes two same-named globals"
        );
    }

    #[test]
    fn a_globals_mangling_policy_selects_its_exact_symbol() {
        let module = path(&["pkg", "counter"]);

        let mut forced = declaration("TOTAL");
        forced.mangling = ManglingMode::Forced("exact_external_name".to_string());
        assert_eq!(
            lower_declaration(forced, &module).symbol,
            "exact_external_name"
        );

        let mut disabled = declaration("TOTAL");
        disabled.mangling = ManglingMode::Disabled;
        assert_eq!(lower_declaration(disabled, &module).symbol, "TOTAL");

        let mut enabled = declaration("TOTAL");
        enabled.mangling = ManglingMode::Enabled;
        assert_eq!(
            lower_declaration(enabled, &module).symbol,
            mangle::global_symbol_string(&module, &Ident("TOTAL".to_string()))
        );
    }

    /// A stored value is data even when its type is a function type: it names
    /// its own storage, never the function symbol a foreign binding of the
    /// same type would name.
    #[test]
    fn a_function_typed_global_still_uses_data_symbol_construction() {
        let module = path(&["pkg", "counter"]);

        let mut declaration = declaration("HANDLER");
        declaration.r#type = function_type();
        let ident = declaration.ident.clone();

        assert_eq!(
            lower_declaration(declaration, &module).symbol,
            mangle::global_symbol_string(&module, &ident)
        );
    }
}
