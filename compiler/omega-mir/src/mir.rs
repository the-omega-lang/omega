use crate::body::{MirBody, MirInlineAsm};
use omega_analyzer::annotations::{InlineMode, ManglingMode};
use omega_analyzer::checked::{CheckedField, CheckedParam, ConformanceOwner};
use omega_analyzer::resolved_type::{
    CallingConvention, ConstValue, ResolvedFunctionParam, ResolvedFunctionType, ResolvedType,
};
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
    ForeignBinding(MirForeignBinding),
    ForeignFunction(MirForeignFunctionDef),
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
pub struct MirForeignBinding {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub ident: Ident,
    pub r#type: ResolvedType,
    pub mangling: ManglingMode,
    pub symbol: String,
}

#[derive(Debug, Clone)]
pub struct MirForeignFunctionDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub calling_convention: CallingConvention,
    pub is_variadic: bool,
    pub params: Vec<CheckedParam>,
    pub return_type: ResolvedType,
    pub mangling: ManglingMode,
    pub symbol: String,
    pub linkage: MirLinkage,
    pub body: Option<MirFunctionBody>,
}

impl MirForeignFunctionDef {
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self
                .params
                .iter()
                .map(|p| ResolvedFunctionParam::described(p.ident.clone(), p.r#type.clone()))
                .collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: None,
            calling_convention: self.calling_convention,
        }
    }
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
    pub body: MirFunctionBody,
}

/// A naked function's body is structurally distinct from an ordinary
/// `MirBody`: it carries no locals, parameter homes, or CFG, only the single
/// checked `asm` that is the entire function implementation. Keeping this a
/// separate variant (rather than a flag on `MirBody`) makes "no frame/return
/// machinery" a property the type system enforces instead of one more
/// runtime check every `MirBody` consumer would need to remember.
#[derive(Debug, Clone)]
pub enum MirFunctionBody {
    Normal(MirBody),
    Naked(MirInlineAsm),
}

impl MirFunctionDef {
    pub fn fn_type(&self) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: self
                .params
                .iter()
                .map(|p| ResolvedFunctionParam::described(p.ident.clone(), p.r#type.clone()))
                .collect(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: self.is_variadic,
            self_mode: self.self_mode,
            calling_convention: CallingConvention::Omega,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MirStructDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedField>,
    pub functions: Vec<MirFunctionDef>,
}

#[derive(Debug, Clone)]
pub struct MirUnionDef {
    pub id: omega_hir::HirId,
    pub span: Span,
    pub name: Ident,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<CheckedField>,
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
