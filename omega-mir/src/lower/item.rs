//! Item-level lowering: `CheckedModule -> MirModule`, mechanical for
//! everything except a function's own body (delegated to
//! `crate::lower::function::FunctionLowerer`).

use crate::lower::function::FunctionLowerer;
use crate::mir::{
    MirDeclaration, MirEnumDef, MirExternDeclaration, MirFunctionDef, MirItem, MirModule, MirStructDef, MirUnionDef,
};
use omega_analyzer::checked::{
    CheckedDeclaration, CheckedEnumDef, CheckedExternDeclaration, CheckedFunctionDef, CheckedItem, CheckedModule,
    CheckedStructDef, CheckedUnionDef,
};

pub(crate) fn lower_module(module: CheckedModule) -> MirModule {
    MirModule { id: module.id, items: module.items.into_iter().map(lower_item).collect() }
}

fn lower_item(item: CheckedItem) -> MirItem {
    match item {
        CheckedItem::Declaration(d) => MirItem::Declaration(lower_declaration(d)),
        CheckedItem::ExternDeclaration(d) => MirItem::ExternDeclaration(lower_extern_declaration(d)),
        CheckedItem::FunctionDefinition(f) => MirItem::FunctionDefinition(lower_function_def(f)),
        CheckedItem::Struct(s) => MirItem::Struct(lower_struct_def(s)),
        CheckedItem::Enum(e) => MirItem::Enum(lower_enum_def(e)),
        CheckedItem::Union(u) => MirItem::Union(lower_union_def(u)),
    }
}

fn lower_declaration(decl: CheckedDeclaration) -> MirDeclaration {
    MirDeclaration { id: decl.id, span: decl.span, ident: decl.ident, r#type: decl.r#type }
}

fn lower_extern_declaration(decl: CheckedExternDeclaration) -> MirExternDeclaration {
    MirExternDeclaration { id: decl.id, span: decl.span, ident: decl.ident, r#type: decl.r#type }
}

fn lower_function_def(f: CheckedFunctionDef) -> MirFunctionDef {
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
        extension_target,
    } = f;
    // Lowered against `&params`/`&return_type` (only its own id/type is
    // needed to seed parameter locals and the return slot -- see
    // `FunctionLowerer::lower`) before either is moved into the returned
    // `MirFunctionDef` below, exactly like `CheckedFunctionDef` itself
    // keeps both a `params` list and a body that implicitly references the
    // same parameters via `Storage::Parameter`.
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
        extension_target,
        body: mir_body,
    }
}

fn lower_struct_def(s: CheckedStructDef) -> MirStructDef {
    MirStructDef {
        id: s.id,
        span: s.span,
        name: s.name,
        type_args: s.type_args,
        fields: s.fields,
        functions: s.functions.into_iter().map(lower_function_def).collect(),
    }
}

fn lower_union_def(u: CheckedUnionDef) -> MirUnionDef {
    MirUnionDef {
        id: u.id,
        span: u.span,
        name: u.name,
        type_args: u.type_args,
        fields: u.fields,
        functions: u.functions.into_iter().map(lower_function_def).collect(),
    }
}

fn lower_enum_def(e: CheckedEnumDef) -> MirEnumDef {
    MirEnumDef {
        id: e.id,
        span: e.span,
        name: e.name,
        type_args: e.type_args,
        functions: e.functions.into_iter().map(lower_function_def).collect(),
    }
}
