
use crate::body::MirBody;
use omega_analyzer::annotations::{InlineMode, ManglingMode};
use omega_analyzer::checked::{CheckedParam, ConformanceOwner};
use omega_analyzer::resolved_type::{ConstValue, ResolvedFunctionType, ResolvedType};
use omega_hir::ModuleId;
use omega_parser::prelude::{Ident, SelfMode, Span};

#[derive(Debug, Clone)]
pub struct MirModule {
    pub id: ModuleId,
    pub items: Vec<MirItem>,
}

#[derive(Debug, Clone)]
pub enum MirItem {
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
    pub initial_value: Option<ConstValue>,
}

#[derive(Debug, Clone)]
pub struct MirExternDeclaration {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
    pub mangling: ManglingMode,
    pub symbol: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirLinkage {
    Export,
    Weak,
}

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
    pub conformance_owner: Option<ConformanceOwner>,
    pub primitive_target: Option<ResolvedType>,
    pub symbol: String,
    pub linkage: MirLinkage,
    pub body: MirBody,
}

impl MirFunctionDef {
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self
                .params
                .iter()
                .map(|p| (p.ident.clone(), p.r#type.clone()))
                .collect(),
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

#[derive(Debug, Clone)]
pub struct MirUnionDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedParam>,
    pub functions: Vec<MirFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct MirEnumDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub functions: Vec<MirFunctionDef>,
}
