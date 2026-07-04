use crate::hir::{
    HirAddressOf, HirAssignment, HirDeclaration, HirExprNode, HirExpr, HirExternDeclaration,
    HirFunctionCall, HirFunctionDef, HirItem, HirModule, HirParam, HirPlace, HirPlaceRoot,
    HirProjection, HirStmt, HirStructDef,
};
use crate::ids::{HirIdGen, ModuleId};
use omega_parser::prelude::{
    DeclarationStmt, Expression, ExpressionNode, ExternDeclarationStmt, FunctionDefinitionStmt,
    Ident, RootStatement, RootStatementNode, SimpleSpan, SourceModule, Statement, StatementNode,
    StructStmt, Type,
};

/// Lowers a freshly parsed module into HIR. Infallible: everything this does
/// is a pure structural transform (assigning ids, desugaring `self`-insertion
/// and place-chains) with no rejectable cases -- semantic analysis remains
/// the only pass that can reject a program.
pub fn lower_module(module: ModuleId, ast: &SourceModule) -> HirModule {
    let mut lowerer = Lowerer {
        ids: HirIdGen::new(module),
    };
    let items = ast.nodes.iter().map(|node| lowerer.lower_item(node)).collect();
    HirModule { id: module, items }
}

struct Lowerer {
    ids: HirIdGen,
}

impl Lowerer {
    fn lower_item(&mut self, node: &RootStatementNode) -> HirItem {
        match &node.root_stmt {
            RootStatement::Declaration(decl) => {
                HirItem::Declaration(self.lower_declaration(decl, node.span))
            }
            RootStatement::ExternDeclaration(decl) => {
                HirItem::ExternDeclaration(self.lower_extern_declaration(decl, node.span))
            }
            RootStatement::FunctionDefinition(f) => {
                HirItem::FunctionDefinition(self.lower_function_def(f, node.span, None))
            }
            RootStatement::Struct(s) => HirItem::Struct(self.lower_struct_def(s, node.span)),
        }
    }

    fn lower_stmt(&mut self, node: &StatementNode) -> HirStmt {
        match &node.statement {
            Statement::Declaration(decl) => {
                HirStmt::Declaration(self.lower_declaration(decl, node.span))
            }
            Statement::ExternDeclaration(decl) => {
                HirStmt::ExternDeclaration(self.lower_extern_declaration(decl, node.span))
            }
            Statement::Expression(expr) => HirStmt::Expression(self.lower_expr(expr)),
            Statement::Return(ret) => HirStmt::Return(self.lower_expr(&ret.return_value)),
            Statement::Struct(s) => HirStmt::Struct(self.lower_struct_def(s, node.span)),
        }
    }

    fn lower_declaration(&mut self, decl: &DeclarationStmt, span: SimpleSpan) -> HirDeclaration {
        HirDeclaration {
            id: self.ids.next(),
            span,
            ident: decl.ident.clone(),
            r#type: decl.r#type.clone(),
        }
    }

    fn lower_extern_declaration(
        &mut self,
        decl: &ExternDeclarationStmt,
        span: SimpleSpan,
    ) -> HirExternDeclaration {
        HirExternDeclaration {
            id: self.ids.next(),
            span,
            ident: decl.ident.clone(),
            r#type: decl.r#type.clone(),
        }
    }

    /// `enclosing_struct` is `Some` when lowering a struct method, in which
    /// case a member function's synthetic `self: *StructName` parameter is
    /// inserted here -- this needs no type information (just the flag and the
    /// struct's name), so it belongs in lowering rather than semantic
    /// analysis, which used to do this ad hoc.
    ///
    /// Note: struct methods have no per-function span in the parser's AST
    /// (only the enclosing `RootStatementNode`/`StatementNode` did, and
    /// struct methods were never wrapped in one) -- `span` is the enclosing
    /// struct's span in that case, an approximation but strictly better than
    /// nothing.
    fn lower_function_def(
        &mut self,
        f: &FunctionDefinitionStmt,
        span: SimpleSpan,
        enclosing_struct: Option<&Ident>,
    ) -> HirFunctionDef {
        let mut params = Vec::with_capacity(f.params.len() + 1);
        if f.is_member_function
            && let Some(struct_ident) = enclosing_struct
        {
            params.push(HirParam {
                id: self.ids.next(),
                span,
                ident: Ident("self".to_string()),
                r#type: Type::Pointer(Box::new(Type::Named(struct_ident.clone()))),
            });
        }
        params.extend(f.params.iter().map(|p| self.lower_param(p, span)));

        let body = f.codeblock.0.iter().map(|s| self.lower_stmt(s)).collect();

        HirFunctionDef {
            id: self.ids.next(),
            span,
            name: f.function_name.clone(),
            is_member_function: f.is_member_function,
            params,
            return_type: f.return_type.clone(),
            body,
        }
    }

    fn lower_param(&mut self, param: &DeclarationStmt, span: SimpleSpan) -> HirParam {
        HirParam {
            id: self.ids.next(),
            span,
            ident: param.ident.clone(),
            r#type: param.r#type.clone(),
        }
    }

    fn lower_struct_def(&mut self, s: &StructStmt, span: SimpleSpan) -> HirStructDef {
        let id = self.ids.next();
        let fields = s.fields.iter().map(|f| self.lower_param(f, span)).collect();
        let functions = s
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, span, Some(&s.ident)))
            .collect();

        HirStructDef {
            id,
            span,
            name: s.ident.clone(),
            fields,
            functions,
        }
    }

    fn lower_expr(&mut self, node: &ExpressionNode) -> HirExprNode {
        match &node.expression {
            Expression::Ident(_)
            | Expression::FieldAccess(_)
            | Expression::Index(_)
            | Expression::Deref(_) => {
                let place = self.lower_place_chain(node);
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Place(place),
                }
            }
            Expression::Number(n) => HirExprNode {
                id: self.ids.next(),
                span: node.span,
                expr: HirExpr::Number(n.clone()),
            },
            Expression::String(s) => HirExprNode {
                id: self.ids.next(),
                span: node.span,
                expr: HirExpr::String(s.clone()),
            },
            Expression::Codeblock(cb) => {
                let stmts = cb.0.iter().map(|s| self.lower_stmt(s)).collect();
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Codeblock(stmts),
                }
            }
            Expression::FunctionCall(call) => {
                let callee = Box::new(self.lower_expr(&call.callee));
                let args = call.args.iter().map(|a| self.lower_expr(a)).collect();
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::FunctionCall(HirFunctionCall { callee, args }),
                }
            }
            Expression::Assignment(assign) => {
                let target = Box::new(self.lower_expr(&assign.target));
                let value = Box::new(self.lower_expr(&assign.value));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Assignment(HirAssignment { target, value }),
                }
            }
            Expression::AddressOf(addr) => {
                let base = Box::new(self.lower_expr(&addr.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::AddressOf(HirAddressOf { base }),
                }
            }
        }
    }

    /// Flattens the parser's nested `FieldAccessExpr`/`IndexExpr` chains
    /// (built left-to-right by postfix folding, e.g. `a.b.c` is
    /// `((a).b).c`) into one `HirPlace` with a flat `Vec<HirProjection>`, in
    /// source order. The parser itself has no idea any of this denotes an
    /// addressable location -- `FieldAccess`/`Index`/`Ident` are just plain
    /// expression-forming constructs to it (see `omega_parser::syntax::expression`).
    /// Recognizing that a chain of them rooted in an identifier (or some other
    /// base expression) is a "place" is entirely this function's job, and it
    /// replaces `analyze_place`'s old "hacky mutation" approach of building
    /// the place incrementally in a shared side-table.
    fn lower_place_chain(&mut self, expr: &ExpressionNode) -> HirPlace {
        match &expr.expression {
            Expression::Ident(ident) => HirPlace {
                root: HirPlaceRoot::Ident(ident.clone()),
                projections: vec![],
            },
            Expression::FieldAccess(access) => {
                let mut place = self.lower_place_chain(&access.base);
                place
                    .projections
                    .push(HirProjection::FieldAccess(access.field.clone()));
                place
            }
            Expression::Index(index_expr) => {
                let mut place = self.lower_place_chain(&index_expr.base);
                let index = Box::new(self.lower_expr(&index_expr.index));
                place.projections.push(HirProjection::Index(index));
                place
            }
            Expression::Deref(deref) => {
                let mut place = self.lower_place_chain(&deref.base);
                place.projections.push(HirProjection::Deref);
                place
            }
            // Base isn't syntactically a place (e.g. `foo().bar`) -- root is
            // just the lowered expression itself.
            _ => HirPlace {
                root: HirPlaceRoot::Expr(Box::new(self.lower_expr(expr))),
                projections: vec![],
            },
        }
    }
}
