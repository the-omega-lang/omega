pub mod declaration;
pub mod defer;
pub mod extern_declaration;
pub mod for_stmt;
pub mod function_definition;
pub mod import;
pub mod macro_definition;
pub mod r#return;
pub mod r#struct;
pub mod walrus;
pub mod while_stmt;

use crate::ast::expression::{ExpressionNode, macro_invocation::MacroInvocationExpr};
use crate::ast::statement::{
    declaration::DeclarationStmt, defer::DeferStmt, extern_declaration::ExternDeclarationStmt,
    for_stmt::ForStmt, function_definition::FunctionDefinitionStmt, import::ImportStmt,
    macro_definition::MacroDefStmt, r#return::ReturnStmt, r#struct::StructStmt,
    walrus::WalrusStmt, while_stmt::WhileStmt,
};
use crate::diagnostics::Span;

// Top level/global scope statements
#[derive(Debug, Clone)]
pub enum RootStatement {
    Declaration(DeclarationStmt),
    ExternDeclaration(ExternDeclarationStmt),
    FunctionDefinition(FunctionDefinitionStmt),
    Struct(StructStmt),
    Import(ImportStmt),
    /// Expanded away entirely (along with `MacroInvocation` below) by
    /// `omega_parser::macros::expand` before HIR lowering ever runs -- see
    /// `MacroDefStmt`'s doc comment.
    MacroDefinition(MacroDefStmt),
    /// `name!(arg, ...);` in item position -- only valid for an
    /// `items`-output macro (see `MacroOutputKind`); the expansion pass
    /// splices its expansion's items in place of this node.
    MacroInvocation(MacroInvocationExpr),
}

#[derive(Debug, Clone)]
pub struct RootStatementNode {
    pub root_stmt: RootStatement,
    pub span: Span,
}

// Function scope statements
#[derive(Debug, Clone)]
pub enum Statement {
    Declaration(DeclarationStmt),
    /// `ident : type = value;` -- unlike `Walrus`, the type is written down
    /// explicitly, so lowering can desugar this straight into a plain
    /// `Declaration` + assignment pair itself (see `lower_stmt`), with no
    /// need for semantic analysis to infer anything first.
    DeclarationWithInit(DeclarationStmt, ExpressionNode),
    ExternDeclaration(ExternDeclarationStmt),
    Expression(ExpressionNode),
    Return(ReturnStmt),
    /// No label yet (just `break;`/`continue;`) -- analysis already resolves
    /// these against a stack of enclosing loops keyed by identity rather
    /// than always assuming "the innermost one," specifically so a labeled
    /// `break 'outer;` can be added later by changing only how that
    /// resolution picks an entry, with no parser/HIR/codegen rework (see
    /// `Analyzer`'s `loop_stack`).
    Break,
    Continue,
    Struct(StructStmt),
    Walrus(WalrusStmt),
    While(WhileStmt),
    /// Boxed since `ForStmt.init` embeds a bare `Statement` -- without the
    /// indirection here, `Statement` would have infinite size.
    For(Box<ForStmt>),
    /// `defer <statement>;` / `defer { ... }` -- see `DeferStmt`'s doc
    /// comment. Unlike `For`, no extra `Box` is needed at this level:
    /// `DeferStmt` itself already boxes its embedded `Statement`
    /// (`DeferStmt.body: Box<Statement>`), which is what breaks the
    /// recursive-size cycle here.
    Defer(DeferStmt),
}

#[derive(Debug, Clone)]
pub struct StatementNode {
    pub statement: Statement,
    pub span: Span,
}
