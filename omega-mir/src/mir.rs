//! Item-level MIR shapes -- the direct analogue of `omega_analyzer::checked`'s
//! `CheckedModule`/`CheckedItem` family, minus anything control-flow-shaped
//! (that's `crate::body`'s job). A struct/enum/union/extern/global
//! declaration carries no control flow of its own, so these are close to a
//! straight field copy of their `Checked*` counterparts -- only a
//! `FunctionDefinition`'s `body` actually changes shape, from a
//! `CheckedBlock` tree to a [`crate::body::MirBody`] graph.

use crate::body::MirBody;
use omega_analyzer::annotations::{InlineMode, ManglingMode};
use omega_analyzer::checked::CheckedParam;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_hir::ModuleId;
use omega_parser::prelude::{Ident, SelfMode, Span};

#[derive(Debug, Clone)]
pub struct MirModule {
    pub id: ModuleId,
    pub items: Vec<MirItem>,
}

#[derive(Debug, Clone)]
pub enum MirItem {
    /// See `CheckedItem::Declaration`'s doc comment -- still `todo!()` in
    /// codegen (global data storage isn't decided yet); carried through
    /// unchanged so that remains true only in one place.
    Declaration(MirDeclaration),
    ExternDeclaration(MirExternDeclaration),
    FunctionDefinition(MirFunctionDef),
    Struct(MirStructDef),
    Enum(MirEnumDef),
    Union(MirUnionDef),
}

#[derive(Debug, Clone)]
pub struct MirDeclaration {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

#[derive(Debug, Clone)]
pub struct MirExternDeclaration {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
}

/// See `CheckedFunctionDef`'s doc comment -- every field here means exactly
/// what it does there, except `body`, which is now a control-flow graph
/// instead of a tree.
#[derive(Debug, Clone)]
pub struct MirFunctionDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub self_mode: Option<SelfMode>,
    pub is_variadic: bool,
    pub params: Vec<CheckedParam>,
    pub return_type: ResolvedType,
    pub inline: Option<InlineMode>,
    pub mangling: ManglingMode,
    pub extension_target: Option<ResolvedType>,
    pub body: MirBody,
}

impl MirFunctionDef {
    /// Same shape/purpose as `CheckedFunctionDef::fn_type` -- codegen builds
    /// a call/definition signature from this, never from `body` directly.
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self.params.iter().map(|p| (p.ident.clone(), p.r#type.clone())).collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: self.self_mode,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MirStructDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedParam>,
    pub functions: Vec<MirFunctionDef>,
}

/// See `CheckedUnionDef`'s doc comment -- same shape as `MirStructDef`.
#[derive(Debug, Clone)]
pub struct MirUnionDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedParam>,
    pub functions: Vec<MirFunctionDef>,
}

/// See `CheckedEnumDef`'s doc comment -- deliberately functions-only, same
/// reasoning (the tag/header/variant data lives in `ResolvedType::Enum`'s
/// shared cell, not duplicated here).
#[derive(Debug, Clone)]
pub struct MirEnumDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub functions: Vec<MirFunctionDef>,
}
