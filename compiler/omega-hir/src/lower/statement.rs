use super::Lowerer;
use crate::hir::{
    HirAsmDescriptor, HirAsmDescriptorKind, HirBlock, HirBreak, HirContinue, HirDefer, HirFor,
    HirForIn, HirInlineAsm, HirLoop, HirStmt, HirWalrusDeclaration, HirWhile,
};
use omega_parser::prelude::{
    AsmDescriptorKind, CodeblockExpr, Span, Statement, StatementNode,
};

impl Lowerer {
    fn lower_stmt(&mut self, node: &StatementNode) -> HirStmt {
        self.lower_statement(&node.statement, node.span)
    }

    fn lower_statement(&mut self, statement: &Statement, span: Span) -> HirStmt {
        match statement {
            Statement::Declaration(decl) => HirStmt::Declaration(self.lower_declaration(decl)),
            Statement::DeclarationWithInit(decl, value) => {
                HirStmt::DeclarationWithInit(self.lower_declaration(decl), self.lower_expr(value))
            }
            Statement::ExternDeclaration(decl) => {
                HirStmt::ExternDeclaration(self.lower_extern_declaration(decl, span))
            }
            Statement::Expression(expr) => HirStmt::Expression(self.lower_expr(expr)),
            Statement::MacroInvocation(_) => unreachable!(
                "statement macro invocations are replaced by their expansion by \
                 omega_parser::macros::expand before lower_module runs"
            ),
            Statement::Return(ret) => HirStmt::Return(self.lower_expr(&ret.return_value)),
            Statement::Break => HirStmt::Break(HirBreak {
                id: self.ids.next(),
                span,
            }),
            Statement::Continue => HirStmt::Continue(HirContinue {
                id: self.ids.next(),
                span,
            }),
            Statement::Walrus(w) => HirStmt::WalrusDeclaration(HirWalrusDeclaration {
                id: self.ids.next(),
                span,
                ident: w.ident.clone(),
                origin: w.origin,
                value: self.lower_expr(&w.value),
                mutable: w.mutable,
                comp: w.comp,
            }),
            Statement::While(w) => HirStmt::While(HirWhile {
                id: self.ids.next(),
                span,
                condition: self.lower_expr(&w.condition),
                body: self.lower_block(&w.body),
            }),
            Statement::Loop(l) => HirStmt::Loop(HirLoop {
                id: self.ids.next(),
                span,
                body: self.lower_block(&l.body),
            }),
            Statement::For(f) => {
                let init = f
                    .init
                    .as_ref()
                    .map(|statement| vec![self.lower_statement(statement, span)])
                    .unwrap_or_default();
                HirStmt::For(HirFor {
                    id: self.ids.next(),
                    span,
                    init,
                    condition: f.condition.as_ref().map(|expr| self.lower_expr(expr)),
                    post: f.post.as_ref().map(|expr| self.lower_expr(expr)),
                    body: self.lower_block(&f.body),
                })
            }
            Statement::ForIn(f) => HirStmt::ForIn(HirForIn {
                id: self.ids.next(),
                span,
                mutable: f.mutable,
                binding: f.binding.clone(),
                binding_type: f.binding_type.clone(),
                iterator: self.lower_expr(&f.iterator),
                body: self.lower_block(&f.body),
            }),
            Statement::Defer(d) => HirStmt::Defer(HirDefer {
                id: self.ids.next(),
                span,
                body: HirBlock {
                    stmts: vec![self.lower_statement(&d.body, span)],
                    tail: None,
                    span,
                },
            }),
            Statement::InlineAsm(asm) => HirStmt::InlineAsm(HirInlineAsm {
                id: self.ids.next(),
                span,
                descriptors: asm
                    .descriptors
                    .iter()
                    .map(|d| self.lower_asm_descriptor(d))
                    .collect(),
                body: asm.body.clone(),
                body_span: asm.body_span,
            }),
        }
    }

    fn lower_asm_descriptor(
        &mut self,
        descriptor: &omega_parser::prelude::AsmDescriptorNode,
    ) -> HirAsmDescriptor {
        let kind = match &descriptor.kind {
            AsmDescriptorKind::Reg { expr, physical } => HirAsmDescriptorKind::Reg {
                expr: self.lower_expr(expr),
                physical: physical.clone(),
            },
            AsmDescriptorKind::Const { name, origin } => HirAsmDescriptorKind::Const {
                name: name.clone(),
                origin: *origin,
            },
            AsmDescriptorKind::Clobber { register } => HirAsmDescriptorKind::Clobber {
                register: register.clone(),
            },
        };
        HirAsmDescriptor {
            id: self.ids.next(),
            span: descriptor.span,
            kind,
        }
    }

    pub(super) fn lower_block(&mut self, block: &CodeblockExpr) -> HirBlock {
        HirBlock {
            stmts: block
                .statements
                .iter()
                .map(|statement| self.lower_stmt(statement))
                .collect(),
            tail: block
                .tail
                .as_ref()
                .map(|expr| Box::new(self.lower_expr(expr))),
            span: block.span,
        }
    }
}
