use crate::ast::annotation::AnnotationNode;
use crate::ast::expression::{CodeblockExpr, ExpressionNode, MacroInvocationExpr};
use crate::ast::generics::GenericParam;
use crate::ast::identifier::{Ident, Origin};
use crate::ast::self_mode::SelfMode;
use crate::ast::r#type::{FunctionType, Param, Type};
use crate::ast::visibility::Visibility;
use crate::diagnostics::Span;

#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ExternDeclaration(ExternDeclarationStmt),
    Expression(ExpressionNode),
    MacroInvocation(MacroInvocationExpr),
    Return(ReturnStmt),
    Break,
    Continue,
    Walrus(WalrusStmt),
    While(WhileStmt),
    Loop(LoopStmt),
    For(Box<ForStmt>),
    ForIn(Box<ForInStmt>),
    Defer(DeferStmt),
}

#[derive(Debug, Clone)]
pub struct StatementNode {
    pub statement: Statement,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct DeclarationStmt {
    pub ident: Ident,
    pub name_span: Span,
    pub span: Span,
    pub origin: Origin,
    pub r#type: Type,
    pub mutable: bool,
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct ExternDeclarationStmt {
    pub ident: Ident,
    pub r#type: Type,
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct ReturnStmt {
    pub return_value: ExpressionNode,
}

#[derive(Debug, Clone)]
pub struct WalrusStmt {
    pub ident: Ident,
    pub origin: Origin,
    pub value: ExpressionNode,
    pub mutable: bool,
    pub comp: bool,
    pub visibility: Visibility,
}

#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub condition: ExpressionNode,
    pub body: CodeblockExpr,
}

#[derive(Debug, Clone)]
pub struct LoopStmt {
    pub body: CodeblockExpr,
}

#[derive(Debug, Clone)]
pub struct ForStmt {
    pub init: Option<Statement>,
    pub condition: Option<ExpressionNode>,
    pub post: Option<ExpressionNode>,
    pub body: CodeblockExpr,
}

#[derive(Debug, Clone)]
pub struct ForInStmt {
    pub mutable: bool,
    pub binding: Ident,
    pub binding_type: Option<Type>,
    pub iterator: ExpressionNode,
    pub body: CodeblockExpr,
}

#[derive(Debug, Clone)]
pub struct DeferStmt {
    pub body: Box<Statement>,
}

#[derive(Debug, Clone)]
pub struct FunctionDefinitionStmt {
    pub annotations: Vec<AnnotationNode>,
    pub visibility: Visibility,
    pub ident: Ident,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub generics: Vec<GenericParam>,
    pub self_mode: Option<SelfMode>,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub codeblock: CodeblockExpr,
}

impl FunctionDefinitionStmt {
    pub fn function_type(&self) -> FunctionType {
        FunctionType {
            params: self.params.clone(),
            return_type: Box::new(self.return_type.clone()),
            is_variadic: false,
            self_mode: self.self_mode,
        }
    }
}
