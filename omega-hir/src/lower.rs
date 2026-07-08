use crate::hir::{
    HirAddressOf, HirAssignment, HirBinaryOp, HirBlock, HirBreak, HirContinue, HirDeclaration,
    HirDefer, HirExprNode, HirExpr, HirExternDeclaration, HirFor, HirFunctionCall, HirFunctionDef,
    HirIf, HirImport, HirItem, HirModule, HirParam, HirPlace, HirPlaceRoot, HirProjection,
    HirSlice, HirStmt, HirStructDef, HirWalrusDeclaration, HirWhile,
};
use crate::ids::{HirIdGen, ModuleId};
use omega_parser::prelude::{
    CodeblockExpr, DeclarationStmt, Expression, ExpressionNode, ExternDeclarationStmt,
    FunctionDefinitionStmt, Ident, RootStatement, RootStatementNode, SimpleSpan, SourceModule,
    Statement, StatementNode, StructStmt, Type,
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
            RootStatement::Import(import) => HirItem::Import(HirImport {
                id: self.ids.next(),
                span: node.span,
                path: import.path.clone(),
            }),
            RootStatement::MacroDefinition(_) | RootStatement::MacroInvocation(_) => {
                unreachable!(
                    "macros are fully expanded (definitions removed, invocations replaced by \
                     their expansion) by omega_parser::macros::expand before lower_module runs"
                )
            }
        }
    }

    fn lower_stmt(&mut self, node: &StatementNode) -> Vec<HirStmt> {
        self.lower_statement(&node.statement, node.span)
    }

    /// Most statements lower into exactly one `HirStmt`; `ident : type =
    /// value;` lowers into two (a plain `Declaration` followed by an
    /// assignment expression statement) -- unlike `Walrus`, this needs no
    /// analysis-time desugaring, since the type is already written down
    /// here, so lowering can do it directly.
    ///
    /// Split out from `lower_stmt` (which just supplies `node.span`) so a
    /// `for` loop's init clause -- a bare `Statement` with no
    /// `StatementNode` span of its own, since it's parsed without the
    /// semicolon/wrapping a real statement normally comes with -- can reuse
    /// this same logic against the enclosing `for` statement's span, the
    /// same approximation `lower_function_def` already makes for struct
    /// methods that have no span of their own either.
    fn lower_statement(&mut self, statement: &Statement, span: SimpleSpan) -> Vec<HirStmt> {
        match statement {
            Statement::Declaration(decl) => {
                vec![HirStmt::Declaration(self.lower_declaration(decl, span))]
            }
            Statement::DeclarationWithInit(decl, value) => {
                let hir_decl = self.lower_declaration(decl, span);
                let target = HirExprNode {
                    id: self.ids.next(),
                    span,
                    expr: HirExpr::Place(HirPlace {
                        root: HirPlaceRoot::Path(decl.ident.clone().into()),
                        projections: vec![],
                    }),
                };
                let assignment = HirExprNode {
                    id: self.ids.next(),
                    span,
                    expr: HirExpr::Assignment(HirAssignment {
                        target: Box::new(target),
                        value: Box::new(self.lower_expr(value)),
                    }),
                };
                vec![HirStmt::Declaration(hir_decl), HirStmt::Expression(assignment)]
            }
            Statement::ExternDeclaration(decl) => {
                vec![HirStmt::ExternDeclaration(self.lower_extern_declaration(decl, span))]
            }
            Statement::Expression(expr) => vec![HirStmt::Expression(self.lower_expr(expr))],
            Statement::Return(ret) => vec![HirStmt::Return(self.lower_expr(&ret.return_value))],
            Statement::Break => vec![HirStmt::Break(HirBreak { id: self.ids.next(), span })],
            Statement::Continue => vec![HirStmt::Continue(HirContinue { id: self.ids.next(), span })],
            Statement::Struct(s) => vec![HirStmt::Struct(self.lower_struct_def(s, span))],
            Statement::Walrus(w) => vec![HirStmt::WalrusDeclaration(HirWalrusDeclaration {
                id: self.ids.next(),
                span,
                ident: w.ident.clone(),
                value: self.lower_expr(&w.value),
            })],
            Statement::While(w) => vec![HirStmt::While(HirWhile {
                id: self.ids.next(),
                span,
                condition: self.lower_expr(&w.condition),
                body: self.lower_block(&w.body),
            })],
            Statement::For(f) => {
                let init = f
                    .init
                    .as_ref()
                    .map(|s| self.lower_statement(s, span))
                    .unwrap_or_default();
                let condition = f.condition.as_ref().map(|c| self.lower_expr(c));
                let post = f.post.as_ref().map(|p| self.lower_expr(p));
                let body = self.lower_block(&f.body);
                vec![HirStmt::For(HirFor { id: self.ids.next(), span, init, condition, post, body })]
            }
            Statement::Defer(d) => {
                let body_stmts = self.lower_statement(&d.body, span);
                vec![HirStmt::Defer(HirDefer {
                    id: self.ids.next(),
                    span,
                    body: HirBlock { stmts: body_stmts, tail: None },
                })]
            }
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

    /// Lowers a `{ stmt; ... tail }` into the equivalent `HirBlock`. Shared
    /// by bare codeblock expressions, `if`/`else` branches, `while`/`for`
    /// bodies, and function bodies -- all identical in shape.
    fn lower_block(&mut self, block: &CodeblockExpr) -> HirBlock {
        let stmts = block.statements.iter().flat_map(|s| self.lower_stmt(s)).collect();
        let tail = block.tail.as_ref().map(|e| Box::new(self.lower_expr(e)));
        HirBlock { stmts, tail }
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
                r#type: Type::Pointer(Box::new(Type::Named(struct_ident.clone().into()))),
            });
        }
        params.extend(f.params.iter().map(|p| self.lower_param(p, span)));

        let body = self.lower_block(&f.codeblock);

        HirFunctionDef {
            id: self.ids.next(),
            span,
            name: f.function_name.clone(),
            generics: f.generics.clone(),
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
            generics: s.generics.clone(),
            fields,
            functions,
        }
    }

    fn lower_expr(&mut self, node: &ExpressionNode) -> HirExprNode {
        match &node.expression {
            Expression::Path(_)
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
            Expression::Bool(b) => HirExprNode {
                id: self.ids.next(),
                span: node.span,
                expr: HirExpr::Bool(b.0),
            },
            Expression::Char(c) => HirExprNode {
                id: self.ids.next(),
                span: node.span,
                expr: HirExpr::Char(c.0),
            },
            Expression::Codeblock(cb) => {
                let block = self.lower_block(cb);
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Codeblock(block) }
            }
            Expression::If(if_expr) => {
                let branches = if_expr
                    .branches
                    .iter()
                    .map(|(cond, block)| (self.lower_expr(cond), self.lower_block(block)))
                    .collect();
                let else_branch = if_expr.else_branch.as_ref().map(|b| self.lower_block(b));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::If(HirIf { branches, else_branch }),
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
            Expression::Negate(neg) => {
                let base = Box::new(self.lower_expr(&neg.base));
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Negate(base) }
            }
            Expression::Increment(incr) => {
                let base = Box::new(self.lower_expr(&incr.base));
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Increment(base) }
            }
            Expression::Decrement(decr) => {
                let base = Box::new(self.lower_expr(&decr.base));
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Decrement(base) }
            }
            Expression::BinaryOp(bin) => {
                let left = Box::new(self.lower_expr(&bin.left));
                let right = Box::new(self.lower_expr(&bin.right));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::BinaryOp(HirBinaryOp { op: bin.op, left, right }),
                }
            }
            Expression::ArrayLiteral(lit) => {
                let elements = lit.elements.iter().map(|e| self.lower_expr(e)).collect();
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::ArrayLiteral(elements) }
            }
            Expression::Slice(s) => {
                let base = self.lower_place_chain(&s.base);
                let start = s.start.as_ref().map(|e| Box::new(self.lower_expr(e)));
                let end = s.end.as_ref().map(|e| Box::new(self.lower_expr(e)));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Slice(HirSlice { base, start, end }),
                }
            }
            Expression::MacroInvocation(_) => unreachable!(
                "macro invocations are replaced by their expansion by \
                 omega_parser::macros::expand before lower_module runs"
            ),
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
            Expression::Path(path) => HirPlace {
                root: HirPlaceRoot::Path(path.clone()),
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
