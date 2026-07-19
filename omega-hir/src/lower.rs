use crate::hir::{
    HirAddressOf, HirAssignment, HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirBinaryOp, HirBlock,
    HirBreak, HirCast,
    HirCompoundAssign, HirContinue,
    HirDeclaration, HirDefer, HirEnumDef, HirEnumVariant, HirExprNode, HirExpr,
    HirExternDeclaration, HirFor, HirFunctionCall, HirFunctionDef, HirGenericParam,
    HirIf, HirImport, HirItem, HirMatch, HirMatchArm, HirModule, HirParam, HirPattern, HirPlace,
    HirPlaceRoot, HirProjection, HirRange, HirSlice, HirSpecDef, HirSpecFunction, HirStmt,
    HirStructDef, HirStructLiteral, HirStructLiteralField, HirUnionDef, HirWalrusDeclaration, HirWhile,
};
use crate::ids::{HirIdGen, ModuleId};
use omega_parser::prelude::{
    AnnotationArg, AnnotationNode, AnnotationValue, CodeblockExpr, DeclarationStmt, EnumStmt, Expression,
    ExpressionNode,
    ExternDeclarationStmt,
    FunctionDefinitionStmt, GenericParam, Ident, Item, ItemNode, Path, Pattern, RangeExpr, SelfMode, Span,
    SourceModule, SpecFunctionStmt, SpecStmt, Statement, StatementNode, StructStmt, Type, UnionStmt,
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
    fn lower_item(&mut self, node: &ItemNode) -> HirItem {
        match &node.item {
            Item::Declaration(decl) => {
                HirItem::Declaration(self.lower_declaration(decl, node.span))
            }
            Item::ExternDeclaration(decl) => {
                HirItem::ExternDeclaration(self.lower_extern_declaration(decl, node.span))
            }
            Item::FunctionDefinition(f) => {
                HirItem::FunctionDefinition(self.lower_function_def(f, node.span, None))
            }
            Item::Struct(s) => HirItem::Struct(self.lower_struct_def(s, node.span)),
            Item::Enum(e) => HirItem::Enum(self.lower_enum_def(e, node.span)),
            Item::Union(u) => HirItem::Union(self.lower_union_def(u, node.span)),
            Item::Spec(sp) => HirItem::Spec(self.lower_spec_def(sp, node.span)),
            Item::Import(import) => HirItem::Import(HirImport {
                id: self.ids.next(),
                span: node.span,
                annotations: Self::lower_annotations(&import.annotations),
                root: import.root,
                path: import.path.clone(),
            }),
            Item::MacroDefinition(_) | Item::MacroInvocation(_) => {
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
    fn lower_statement(&mut self, statement: &Statement, span: Span) -> Vec<HirStmt> {
        match statement {
            Statement::Declaration(decl) => {
                vec![HirStmt::Declaration(self.lower_declaration(decl, span))]
            }
            Statement::DeclarationWithInit(decl, value) => {
                let hir_decl = self.lower_declaration(decl, span);
                vec![HirStmt::DeclarationWithInit(hir_decl, self.lower_expr(value))]
            }
            Statement::ExternDeclaration(decl) => {
                vec![HirStmt::ExternDeclaration(self.lower_extern_declaration(decl, span))]
            }
            Statement::Expression(expr) => vec![HirStmt::Expression(self.lower_expr(expr))],
            Statement::Return(ret) => vec![HirStmt::Return(self.lower_expr(&ret.return_value))],
            Statement::Break => vec![HirStmt::Break(HirBreak { id: self.ids.next(), span })],
            Statement::Continue => vec![HirStmt::Continue(HirContinue { id: self.ids.next(), span })],
            Statement::Walrus(w) => vec![HirStmt::WalrusDeclaration(HirWalrusDeclaration {
                id: self.ids.next(),
                span,
                ident: w.ident.clone(),
                value: self.lower_expr(&w.value),
                mutable: w.mutable,
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

    fn lower_declaration(&mut self, decl: &DeclarationStmt, span: Span) -> HirDeclaration {
        HirDeclaration {
            id: self.ids.next(),
            span,
            ident: decl.ident.clone(),
            r#type: decl.r#type.clone(),
            mutable: decl.mutable,
        }
    }

    fn lower_extern_declaration(
        &mut self,
        decl: &ExternDeclarationStmt,
        span: Span,
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
    /// (only the enclosing `ItemNode`/`StatementNode` did, and
    /// struct methods were never wrapped in one) -- `span` is the enclosing
    /// struct's span in that case, an approximation but strictly better than
    /// nothing.
    fn lower_function_def(
        &mut self,
        f: &FunctionDefinitionStmt,
        span: Span,
        enclosing_struct: Option<&Ident>,
    ) -> HirFunctionDef {
        let mut params = Vec::with_capacity(f.params.len() + 1);
        if let Some(struct_ident) = enclosing_struct
            && let Some(p) = self.self_param(f.self_mode, struct_ident, span)
        {
            params.push(p);
        }
        params.extend(f.params.iter().map(|p| self.lower_param(p, span)));

        let mut body = self.lower_block(&f.codeblock);
        if f.self_mode == Some(SelfMode::MutValue) {
            body.stmts.insert(0, self.self_shadow_stmt(span));
        }

        HirFunctionDef {
            id: self.ids.next(),
            span,
            annotations: Self::lower_annotations(&f.annotations),
            name: f.ident.clone(),
            generics: Self::lower_generics(&f.generics),
            self_mode: f.self_mode,
            params,
            return_type: f.return_type.clone(),
            body,
        }
    }

    /// The synthetic `self` parameter every member function gets, shared
    /// between ordinary struct/enum/union methods (`type_name` = the owning
    /// type's own name) and spec functions (`type_name` = the literal
    /// identifier `Self`, resolved later like any other in-scope type name
    /// -- see `omega_analyzer::analysis::Analyzer`'s `Self` seeding). `None`
    /// for a non-member function, so callers can push the result
    /// unconditionally via `if let Some(p) = ...`. The built type depends on
    /// `self_mode`: `Type::Pointer(Named(type_name), mutable)` for
    /// `Pointer`/`MutPointer`, or plain `Type::Named(type_name)` for
    /// `Value`/`MutValue` -- `MutValue`'s local mutability is *not*
    /// represented here at all, since parameters can never be mutable
    /// bindings; see `self_shadow_stmt`.
    fn self_param(&mut self, self_mode: Option<SelfMode>, type_name: &Ident, span: Span) -> Option<HirParam> {
        let mode = self_mode?;
        let r#type = if mode.is_pointer() {
            Type::Pointer(Box::new(Type::Named(type_name.clone().into())), mode.is_mutable())
        } else {
            Type::Named(type_name.clone().into())
        };
        Some(HirParam { id: self.ids.next(), span, ident: Ident("self".to_string()), r#type })
    }

    /// Desugars `mut self` (by value) into an implicit `mut self := self;`
    /// as the body's first statement -- parameters can never be mutable
    /// bindings themselves (`Analyzer::analyze_param` always declares them
    /// immutable, and codegen has no support for writing into one), but a
    /// parameter can always be *shadowed* by a mutable local of the same
    /// name (the pre-existing, hand-writable `mut x := param;` idiom -- see
    /// `Analyzer::analyze_param`'s doc comment). Auto-generating exactly
    /// that shadow here means `mut self` needs zero new mutability
    /// machinery anywhere downstream: the shadow is just an ordinary
    /// mutable local in a stack slot, ranging over the rest of the body,
    /// which already works.
    fn self_shadow_stmt(&mut self, span: Span) -> HirStmt {
        let self_ident = Ident("self".to_string());
        HirStmt::WalrusDeclaration(HirWalrusDeclaration {
            id: self.ids.next(),
            span,
            ident: self_ident.clone(),
            value: HirExprNode {
                id: self.ids.next(),
                span,
                expr: HirExpr::Place(HirPlace {
                    root: HirPlaceRoot::Path(Path::from(self_ident).into()),
                    projections: vec![],
                }),
            },
            mutable: true,
        })
    }

    /// Mechanical clone of a parsed generics list into HIR's own shape --
    /// bounds stay raw/unresolved, same as everywhere else.
    fn lower_generics(generics: &[GenericParam]) -> Vec<HirGenericParam> {
        generics.iter().map(|g| HirGenericParam { ident: g.ident.clone(), bound: g.bound.clone() }).collect()
    }

    /// Mechanical clone of a parsed annotation list into HIR's own shape --
    /// unvalidated, same as everywhere else (see `HirAnnotation`'s doc
    /// comment).
    fn lower_annotations(annotations: &[AnnotationNode]) -> Vec<HirAnnotation> {
        annotations
            .iter()
            .map(|a| HirAnnotation {
                name: a.name.clone(),
                args: a
                    .args
                    .iter()
                    .map(|arg| match arg {
                        AnnotationArg::Ident(ident) => HirAnnotationArg::Ident(ident.clone()),
                        AnnotationArg::KeyValue(key, AnnotationValue::IntLiteral(value)) => {
                            HirAnnotationArg::KeyValue(key.clone(), HirAnnotationValue::IntLiteral(value.clone()))
                        }
                        AnnotationArg::KeyValue(key, AnnotationValue::Sizeof(r#type)) => {
                            HirAnnotationArg::KeyValue(key.clone(), HirAnnotationValue::Sizeof(r#type.clone()))
                        }
                    })
                    .collect(),
                span: a.span,
            })
            .collect()
    }

    fn lower_spec_def(&mut self, sp: &SpecStmt, span: Span) -> HirSpecDef {
        let id = self.ids.next();
        let generics = Self::lower_generics(&sp.generics);
        let dependencies = sp.dependencies.clone();
        let functions = sp.functions.iter().map(|f| self.lower_spec_function(f, span)).collect();

        HirSpecDef { id, span, name: sp.ident.clone(), generics, dependencies, functions }
    }

    /// `Self` is the type-name lowering hands to `self_param` here --
    /// meaningless until a concrete implementor is known, resolved the same
    /// way as any other in-scope type name (see `HirSpecDef`'s doc
    /// comment).
    fn lower_spec_function(&mut self, f: &SpecFunctionStmt, span: Span) -> HirSpecFunction {
        let self_type_name = Ident("Self".to_string());
        let mut params = Vec::with_capacity(f.params.len() + 1);
        if let Some(p) = self.self_param(f.self_mode, &self_type_name, span) {
            params.push(p);
        }
        params.extend(f.params.iter().map(|p| self.lower_param(p, span)));

        let mut body = f.body.as_ref().map(|b| self.lower_block(b));
        if f.self_mode == Some(SelfMode::MutValue)
            && let Some(body) = &mut body
        {
            body.stmts.insert(0, self.self_shadow_stmt(span));
        }

        HirSpecFunction {
            id: self.ids.next(),
            span,
            name: f.ident.clone(),
            self_mode: f.self_mode,
            params,
            return_type: f.return_type.clone(),
            body,
        }
    }

    fn lower_param(&mut self, param: &DeclarationStmt, span: Span) -> HirParam {
        HirParam {
            id: self.ids.next(),
            span,
            ident: param.ident.clone(),
            r#type: param.r#type.clone(),
        }
    }

    fn lower_struct_def(&mut self, s: &StructStmt, span: Span) -> HirStructDef {
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
            annotations: Self::lower_annotations(&s.annotations),
            name: s.ident.clone(),
            generics: Self::lower_generics(&s.generics),
            implements: s.implements.clone(),
            fields,
            functions,
        }
    }

    /// Same treatment as `lower_struct_def` -- member functions get their
    /// synthetic `self: *UnionName` inserted by `lower_function_def`.
    fn lower_union_def(&mut self, u: &UnionStmt, span: Span) -> HirUnionDef {
        let id = self.ids.next();
        let fields = u.fields.iter().map(|f| self.lower_param(f, span)).collect();
        let functions = u
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, span, Some(&u.ident)))
            .collect();

        HirUnionDef {
            id,
            span,
            annotations: Self::lower_annotations(&u.annotations),
            name: u.ident.clone(),
            generics: Self::lower_generics(&u.generics),
            implements: u.implements.clone(),
            fields,
            functions,
        }
    }

    /// Same treatment as `lower_struct_def` -- member functions get their
    /// synthetic `self: *EnumName` inserted by `lower_function_def`, exactly
    /// like a struct's. Header entries keep their own real spans (the parser
    /// records them -- position-sensitive `tag` rules deserve precise
    /// errors); variant body fields and the shared dynamic fields (no
    /// position-sensitive rules of their own) inherit the enum's/their
    /// variant's span, the same approximation struct fields make with their
    /// struct's.
    fn lower_enum_def(&mut self, e: &EnumStmt, span: Span) -> HirEnumDef {
        let id = self.ids.next();
        let header = e
            .header
            .iter()
            .map(|h| HirParam {
                id: self.ids.next(),
                span: h.span,
                ident: h.ident.clone(),
                r#type: h.r#type.clone(),
            })
            .collect();
        let dynamic_fields = e.dynamic_fields.iter().map(|f| self.lower_param(f, span)).collect();
        let variants = e
            .variants
            .iter()
            .map(|v| HirEnumVariant {
                id: self.ids.next(),
                span: v.span,
                name: v.ident.clone(),
                args: v.args.iter().map(|a| self.lower_expr(a)).collect(),
                fields: v.fields.iter().map(|f| self.lower_param(f, v.span)).collect(),
            })
            .collect();
        let functions = e
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, span, Some(&e.ident)))
            .collect();

        HirEnumDef {
            id,
            span,
            annotations: Self::lower_annotations(&e.annotations),
            name: e.ident.clone(),
            generics: Self::lower_generics(&e.generics),
            implements: e.implements.clone(),
            header,
            dynamic_fields,
            variants,
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
            Expression::ByteString(s) => HirExprNode {
                id: self.ids.next(),
                span: node.span,
                expr: HirExpr::ByteString(s.clone()),
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
            Expression::CompoundAssign(assign) => {
                let target = Box::new(self.lower_expr(&assign.target));
                let value = Box::new(self.lower_expr(&assign.value));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::CompoundAssign(HirCompoundAssign { target, op: assign.op, value }),
                }
            }
            Expression::AddressOf(addr) => {
                let base = Box::new(self.lower_expr(&addr.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::AddressOf(HirAddressOf { base, mutable: addr.mutable }),
                }
            }
            Expression::Negate(neg) => {
                let base = Box::new(self.lower_expr(&neg.base));
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Negate(base) }
            }
            Expression::BitNot(not) => {
                let base = Box::new(self.lower_expr(&not.base));
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::BitNot(base) }
            }
            Expression::Cast(cast) => {
                let base = Box::new(self.lower_expr(&cast.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Cast(HirCast { target: cast.target.clone(), base }),
                }
            }
            Expression::Sizeof(sizeof) => {
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Sizeof(sizeof.r#type.clone()) }
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
            Expression::StructLiteral(lit) => {
                let fields = lit
                    .fields
                    .iter()
                    .map(|f| HirStructLiteralField {
                        name: f.name.clone(),
                        name_span: f.name_span,
                        value: self.lower_expr(&f.value),
                    })
                    .collect();
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::StructLiteral(HirStructLiteral { path: lit.path.clone(), fields }),
                }
            }
            Expression::Slice(s) => {
                let base = self.lower_place_chain(&s.base);
                let range = self.lower_range(&s.range);
                HirExprNode { id: self.ids.next(), span: node.span, expr: HirExpr::Slice(HirSlice { base, range }) }
            }
            Expression::Match(m) => {
                let scrutinee = Box::new(self.lower_expr(&m.scrutinee));
                let arms = m
                    .arms
                    .iter()
                    .map(|arm| HirMatchArm {
                        pattern: self.lower_pattern(&arm.pattern),
                        body: self.lower_expr(&arm.body),
                        span: arm.span,
                    })
                    .collect();
                let else_branch = m.else_branch.as_ref().map(|b| self.lower_block(b));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Match(HirMatch { scrutinee, arms, else_branch }),
                }
            }
            Expression::MacroInvocation(_) => unreachable!(
                "macro invocations are replaced by their expansion by \
                 omega_parser::macros::expand before lower_module runs"
            ),
        }
    }

    /// See `HirRange`'s doc comment -- shared, structural lowering for both
    /// `HirSlice` and `HirPattern::Range`.
    fn lower_range(&mut self, range: &RangeExpr) -> HirRange {
        HirRange {
            start: range.start.as_ref().map(|e| Box::new(self.lower_expr(e))),
            end: range.end.as_ref().map(|e| Box::new(self.lower_expr(e))),
            inclusive: range.inclusive,
            span: range.span,
        }
    }

    fn lower_pattern(&mut self, pattern: &Pattern) -> HirPattern {
        match pattern {
            Pattern::Value(v) => HirPattern::Value(self.lower_expr(v)),
            Pattern::Range(r) => HirPattern::Range(self.lower_range(r)),
        }
    }

    /// Flattens the parser's nested `FieldAccessExpr`/`IndexExpr` chains
    /// (built left-to-right by postfix folding, e.g. `a.b.c` is
    /// `((a).b).c`) into one `HirPlace` with a flat `Vec<HirProjection>`, in
    /// source order. The parser itself has no idea any of this denotes an
    /// addressable location -- `FieldAccess`/`Index`/`Ident` are just plain
    /// expression-forming constructs to it (see `omega_parser::ast::expression`).
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
