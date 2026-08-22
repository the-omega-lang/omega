use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::{CodeblockExpr, ExpressionNode, MacroInvocationExpr};
use crate::ast::generics::GenericParam;
use crate::ast::identifier::{Ident, Path};
use crate::ast::self_mode::SelfMode;
use crate::ast::statement::{
    DeclarationStmt, ExternDeclarationStmt, FunctionDefinitionStmt, WalrusStmt,
};
use crate::ast::r#type::{Param, Type};
use crate::ast::visibility::Visibility;
use crate::diagnostics::Span;
use crate::lexer::Token;

#[derive(Debug, Clone)]
pub enum Item {
    Declaration(DeclarationStmt),
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ExternDeclaration(ExternDeclarationStmt),
    FunctionDefinition(FunctionDefinitionStmt),
    Struct(StructStmt),
    Enum(EnumStmt),
    Union(UnionStmt),
    Spec(SpecStmt),
    Gap(GapStmt),
    Glue(GlueStmt),
    Conform(ConformStmt),
    Primitive(PrimitiveStmt),
    Walrus(WalrusStmt),
    Import(ImportStmt),
    MacroDefinition(MacroDefinitionStmt),
    MacroInvocation(MacroInvocationExpr),
}

#[derive(Debug, Clone)]
pub struct ItemNode {
    pub item: Item,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
    pub is_marker: bool,
}

#[derive(Debug, Clone)]
pub struct UnionStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<DeclarationStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct EnumStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub ident: Ident,
    pub generics: Vec<GenericParam>,
    pub header: Vec<EnumHeaderField>,
    pub dynamic_fields: Vec<DeclarationStmt>,
    pub variants: Vec<EnumVariantStmt>,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct EnumHeaderField {
    pub ident: Ident,
    pub name_span: Span,
    pub r#type: Type,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumVariantStmt {
    pub ident: Ident,
    pub span: Span,
    pub args: Vec<ExpressionNode>,
    pub fields: Vec<DeclarationStmt>,
}

#[derive(Debug, Clone)]
pub struct SpecStmt {
    pub ident: Ident,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub generics: Vec<GenericParam>,
    pub dependencies: Vec<Type>,
    pub functions: Vec<SpecFunctionStmt>,
    pub is_alias: bool,
    pub annotations: Vec<AnnotationNode>,
}

#[derive(Debug, Clone)]
pub struct SpecFunctionStmt {
    pub ident: Ident,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub visibility: Visibility,
    pub explicit_hidden_span: Option<Span>,
    pub self_mode: Option<SelfMode>,
    pub params: Vec<Param>,
    pub is_variadic: bool,
    pub return_type: Type,
    pub body: Option<CodeblockExpr>,
}

#[derive(Debug, Clone)]
pub struct GapStmt {
    pub ident: Ident,
    pub functions: Vec<SpecFunctionStmt>,
}

#[derive(Debug, Clone)]
pub struct GlueStmt {
    pub gap: Path,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct ConformStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub spec: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct PrimitiveStmt {
    pub generics: Vec<GenericParam>,
    pub target: Type,
    pub functions: Vec<FunctionDefinitionStmt>,
}

#[derive(Debug, Clone)]
pub struct ImportStmt {
    pub annotations: Vec<AnnotationNode>,
    pub reveal: bool,
    pub root: ImportRoot,
    pub path: Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRoot {
    TopLevel,
    Root,
    SelfModule,
    /// One or more chained leading `super::` segments; the count is the depth.
    Super(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentKind {
    Expr,
    Type,
    Ident,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: Ident,
    pub kind: FragmentKind,
}

#[derive(Debug, Clone)]
pub struct MacroSignature {
    pub fixed: Vec<MacroParam>,
    pub variadic: Option<MacroParam>,
}

#[derive(Debug, Clone)]
pub enum MacroBodyPiece {
    Token(Token),
    Repetition(MacroRepetition),
}

#[derive(Debug, Clone)]
pub struct MacroRepetition {
    pub separator: Option<Token>,
    pub body: Vec<MacroBodyPiece>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MacroDefinitionStmt {
    pub visibility: Visibility,
    pub name: Ident,
    pub signature: MacroSignature,
    pub body: Vec<MacroBodyPiece>,
    pub defining_module: Vec<Ident>,
}
