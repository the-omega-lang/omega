use crate::hir::{
    HirAddressOf, HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirAssignment, HirBinaryOp,
    HirBlock, HirBreak, HirCast, HirComposeDef, HirCompoundAssign, HirContinue, HirDeclaration,
    HirDefer, HirEnumDef, HirEnumVariant, HirExpr, HirExprNode, HirExternDeclaration, HirFor,
    HirForIn, HirFunctionCall, HirFunctionDef, HirGapDef, HirGapFunction, HirGenericParam,
    HirGlueDef, HirIf, HirImport, HirItem, HirLoop, HirMatch, HirMatchArm, HirModule, HirParam,
    HirPattern, HirPlace, HirPlaceRoot, HirPrimitiveDef, HirProjection, HirRange, HirSlice,
    HirSpecDef, HirSpecFunction, HirStmt, HirStructDef, HirStructLiteral, HirStructLiteralField,
    HirUnionDef, HirWalrusDeclaration, HirWhile,
};
use crate::ids::{HirIdGen, ModuleId};
use omega_parser::prelude::{
    AnnotationArg, AnnotationNode, AnnotationValue, CodeblockExpr, DeclarationStmt, EnumStmt,
    Expression, ExpressionNode, ExternDeclarationStmt, FunctionDefinitionStmt, GenericParam, Ident,
    Item, ItemNode, Path, Pattern, RangeExpr, SelfMode, SourceModule, Span, SpecFunctionStmt,
    SpecStmt, Statement, StatementNode, StructStmt, Type, UnionStmt, Visibility,
};

/// Lowers a freshly parsed module into HIR. Infallible: everything this does
/// is a pure structural transform (assigning ids, desugaring `self`-insertion
/// and place-chains) with no rejectable cases -- semantic analysis remains
/// the only pass that can reject a program.
pub fn lower_module(module: ModuleId, ast: &SourceModule) -> HirModule {
    let mut lowerer = Lowerer {
        ids: HirIdGen::new(module),
    };
    let items = ast
        .nodes
        .iter()
        .map(|node| lowerer.lower_item(node))
        .collect();
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
            Item::DeclarationWithInit(decl, value) => HirItem::DeclarationWithInit(
                self.lower_declaration(decl, node.span),
                self.lower_expr(value),
            ),
            Item::Walrus(w) => HirItem::Walrus(HirWalrusDeclaration {
                id: self.ids.next(),
                span: node.span,
                ident: w.ident.clone(),
                value: self.lower_expr(&w.value),
                mutable: w.mutable,
                comp: w.comp,
                visibility: w.visibility,
            }),
            Item::ExternDeclaration(decl) => {
                HirItem::ExternDeclaration(self.lower_extern_declaration(decl, node.span))
            }
            Item::FunctionDefinition(f) => {
                HirItem::FunctionDefinition(self.lower_function_def(f, node.span, false))
            }
            Item::Struct(s) => HirItem::Struct(self.lower_struct_def(s, node.span)),
            Item::Enum(e) => HirItem::Enum(self.lower_enum_def(e, node.span)),
            Item::Union(u) => HirItem::Union(self.lower_union_def(u, node.span)),
            Item::Spec(sp) => HirItem::Spec(self.lower_spec_def(sp, node.span)),
            Item::Gap(gap) => HirItem::Gap(HirGapDef {
                id: self.ids.next(),
                span: node.span,
                name: gap.ident.clone(),
                functions: gap
                    .functions
                    .iter()
                    .map(|f| HirGapFunction {
                        id: self.ids.next(),
                        span: node.span,
                        name: f.ident.clone(),
                        params: f
                            .params
                            .iter()
                            .map(|p| self.lower_param(p, node.span))
                            .collect(),
                        return_type: f.return_type.clone(),
                    })
                    .collect(),
            }),
            Item::Glue(glue) => HirItem::Glue(HirGlueDef {
                id: self.ids.next(),
                span: node.span,
                gap: glue.gap.clone(),
                functions: glue
                    .functions
                    .iter()
                    .map(|f| self.lower_function_def(f, node.span, false))
                    .collect(),
            }),
            Item::Compose(compose) => HirItem::Compose(HirComposeDef {
                id: self.ids.next(),
                span: node.span,
                generics: Self::lower_generics(&compose.generics),
                target: compose.target.clone(),
                spec: compose.spec.clone(),
                functions: compose
                    .functions
                    .iter()
                    .map(|f| self.lower_function_def(f, node.span, true))
                    .collect(),
            }),
            Item::Primitive(primitive) => HirItem::Primitive(HirPrimitiveDef {
                id: self.ids.next(),
                span: node.span,
                generics: Self::lower_generics(&primitive.generics),
                target: primitive.target.clone(),
                functions: primitive
                    .functions
                    .iter()
                    .map(|f| self.lower_function_def(f, node.span, true))
                    .collect(),
            }),
            Item::Import(import) => HirItem::Import(HirImport {
                id: self.ids.next(),
                span: node.span,
                annotations: Self::lower_annotations(&import.annotations),
                reveal: import.reveal,
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
                vec![HirStmt::DeclarationWithInit(
                    hir_decl,
                    self.lower_expr(value),
                )]
            }
            Statement::ExternDeclaration(decl) => {
                vec![HirStmt::ExternDeclaration(
                    self.lower_extern_declaration(decl, span),
                )]
            }
            Statement::Expression(expr) => vec![HirStmt::Expression(self.lower_expr(expr))],
            Statement::MacroInvocation(_) => unreachable!(
                "statement macro invocations are replaced by their expansion by \
                 omega_parser::macros::expand before lower_module runs"
            ),
            Statement::Return(ret) => vec![HirStmt::Return(self.lower_expr(&ret.return_value))],
            Statement::Break => vec![HirStmt::Break(HirBreak {
                id: self.ids.next(),
                span,
            })],
            Statement::Continue => vec![HirStmt::Continue(HirContinue {
                id: self.ids.next(),
                span,
            })],
            Statement::Walrus(w) => vec![HirStmt::WalrusDeclaration(HirWalrusDeclaration {
                id: self.ids.next(),
                span,
                ident: w.ident.clone(),
                value: self.lower_expr(&w.value),
                mutable: w.mutable,
                comp: w.comp,
                visibility: Visibility::default(),
            })],
            Statement::While(w) => vec![HirStmt::While(HirWhile {
                id: self.ids.next(),
                span,
                condition: self.lower_expr(&w.condition),
                body: self.lower_block(&w.body),
            })],
            Statement::Loop(l) => vec![HirStmt::Loop(HirLoop {
                id: self.ids.next(),
                span,
                body: self.lower_block(&l.body),
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
                vec![HirStmt::For(HirFor {
                    id: self.ids.next(),
                    span,
                    init,
                    condition,
                    post,
                    body,
                })]
            }
            Statement::ForIn(f) => vec![HirStmt::ForIn(HirForIn {
                id: self.ids.next(),
                span,
                mutable: f.mutable,
                binding: f.binding.clone(),
                binding_type: f.binding_type.clone(),
                iterator: self.lower_expr(&f.iterator),
                body: self.lower_block(&f.body),
            })],
            Statement::Defer(d) => {
                let body_stmts = self.lower_statement(&d.body, span);
                vec![HirStmt::Defer(HirDefer {
                    id: self.ids.next(),
                    span,
                    body: HirBlock {
                        stmts: body_stmts,
                        tail: None,
                    },
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
            visibility: decl.visibility,
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
            visibility: decl.visibility,
        }
    }

    /// Lowers a `{ stmt; ... tail }` into the equivalent `HirBlock`. Shared
    /// by bare codeblock expressions, `if`/`else` branches, `while`/`for`
    /// bodies, and function bodies -- all identical in shape.
    fn lower_block(&mut self, block: &CodeblockExpr) -> HirBlock {
        let stmts = block
            .statements
            .iter()
            .flat_map(|s| self.lower_stmt(s))
            .collect();
        let tail = block.tail.as_ref().map(|e| Box::new(self.lower_expr(e)));
        HirBlock { stmts, tail }
    }

    /// `is_member` is `true` when lowering a struct/union/enum method, in
    /// which case a member function's synthetic `self: *Self` parameter is
    /// inserted here -- this needs no type information beyond the flag, so
    /// it belongs in lowering rather than semantic analysis, which used to
    /// do this ad hoc.
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
        is_member: bool,
    ) -> HirFunctionDef {
        let mut params = Vec::with_capacity(f.params.len() + 1);
        if is_member && let Some(p) = self.self_param(f.self_mode, span) {
            params.push(p);
        }
        params.extend(f.params.iter().map(|p| self.lower_param(p, span)));

        let mut body = self.lower_block(&f.codeblock);
        if f.self_mode == Some(SelfMode::MutValue) {
            body.stmts.insert(0, self.self_shadow_stmt(span));
        }

        let mut generics = Self::lower_generics(&f.generics);
        Self::desugar_spec_static_params(&mut params, &mut generics);

        HirFunctionDef {
            id: self.ids.next(),
            span,
            annotations: Self::lower_annotations(&f.annotations),
            visibility: f.visibility,
            name: f.ident.clone(),
            generics,
            self_mode: f.self_mode,
            params,
            return_type: f.return_type.clone(),
            body,
        }
    }

    /// `f(x: spec Foo)` sugar -- an implicit, bound generic parameter,
    /// exactly as if the caller had written `f<$ParamN: Foo>(x: $ParamN)`.
    /// Purely mechanical (no semantic decisions: `item_generics`/
    /// `collect_function_signature`/`ensure_item`'s bound-checking all run
    /// completely unmodified afterward, seeing an ordinary bound generic
    /// function) -- which is exactly why this belongs in lowering rather
    /// than analysis, mirroring `self_param`'s identical "no type
    /// information needed, so do it here" reasoning above. `self` can never
    /// be `spec T`-typed (always `Self`), so it's harmless that it's
    /// included in `params`' own indexing here.
    ///
    /// Recurses into the same compound shapes `unify_generic_type`/
    /// `type_references_generics` already do (`*spec Foo`, `[spec Foo]`,
    /// a function-typed parameter's own params/return, ...) rather than
    /// only matching a bare top-level `spec Foo` -- `thing: *spec Speak`
    /// (the common "pass by pointer" idiom this codebase already uses for
    /// explicit bound generics, e.g. `animal: *T` in the specs docs) works
    /// the same way a bare `thing: spec Speak` does.
    ///
    /// Every occurrence gets its *own* fresh generic (never shares one
    /// across two `spec Foo` occurrences) -- matching Rust's `impl Trait`:
    /// `f(a: impl Foo, b: impl Foo)` doesn't require `a`/`b` to be the same
    /// concrete type. `$`-prefixed, matching the `$iter`/`$next` synthetic-
    /// identifier convention the for-in loop desugaring already established
    /// (`$` can't start a user identifier, so this can never collide with a
    /// real generic parameter's name).
    fn desugar_spec_static_params(params: &mut [HirParam], generics: &mut Vec<HirGenericParam>) {
        let mut next = 0usize;
        for param in params.iter_mut() {
            Self::replace_spec_static(&mut param.r#type, &mut next, generics);
        }
    }

    fn replace_spec_static(ty: &mut Type, next: &mut usize, generics: &mut Vec<HirGenericParam>) {
        match ty {
            Type::SpecStatic(bound) => {
                let fresh = Ident(format!("$Param{next}"));
                *next += 1;
                generics.push(HirGenericParam {
                    ident: fresh.clone(),
                    bound: Some((**bound).clone()),
                    default: None,
                });
                *ty = Type::Named(fresh.into());
            }
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => {
                Self::replace_spec_static(inner, next, generics);
            }
            Type::Generic(_, args) => {
                for a in args.iter_mut() {
                    Self::replace_spec_static(a, next, generics);
                }
            }
            Type::Function(f) => {
                for (_, p) in f.params.iter_mut() {
                    Self::replace_spec_static(p, next, generics);
                }
                Self::replace_spec_static(&mut f.return_type, next, generics);
            }
            Type::Named(_) | Type::SpecObject(_, _) => {}
        }
    }

    /// The synthetic `self` parameter every member function gets -- struct/
    /// union/enum methods and spec functions alike, always typed
    /// `Type::Named("Self")` (never the owning type's own literal name).
    /// `None` for a non-member function, so callers can push the result
    /// unconditionally via `if let Some(p) = ...`. The built type depends on
    /// `self_mode`: `Type::Pointer(Named("Self"), mutable)` for
    /// `Pointer`/`MutPointer`, or plain `Type::Named("Self")` for
    /// `Value`/`MutValue` -- `MutValue`'s local mutability is *not*
    /// represented here at all, since parameters can never be mutable
    /// bindings; see `self_shadow_stmt`.
    ///
    /// This is deliberately never the owner's own bare name: `Self` is what
    /// every struct/union/enum/spec method's own analysis substitution
    /// already binds to the concrete owner type (see
    /// `omega_analyzer::analysis::Analyzer`'s `Self` seeding, mirrored at
    /// both `Driver::compute_item` and `Driver::check_item_body`), so
    /// resolving through it needs no further lookup and, critically, never
    /// re-triggers an independent by-name lookup of the owner -- which,
    /// for a *generic* owner, would need its own type arguments supplied
    /// and previously produced a bogus `'Pair' expects 1 type argument(s),
    /// found 0` error for any generic struct/enum with a self-taking
    /// method (a signature-time bug, independent of whether the body
    /// itself references `self`).
    fn self_param(&mut self, self_mode: Option<SelfMode>, span: Span) -> Option<HirParam> {
        let mode = self_mode?;
        let self_type = Ident("Self".to_string());
        let r#type = if mode.is_pointer() {
            Type::Pointer(Box::new(Type::Named(self_type.into())), mode.is_mutable())
        } else {
            Type::Named(self_type.into())
        };
        Some(HirParam {
            id: self.ids.next(),
            span,
            ident: Ident("self".to_string()),
            r#type,
            visibility: Visibility::default(),
        })
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
            comp: false,
            visibility: Visibility::default(),
        })
    }

    /// Mechanical clone of a parsed generics list into HIR's own shape --
    /// bounds and defaults stay raw/unresolved, same as everywhere else.
    fn lower_generics(generics: &[GenericParam]) -> Vec<HirGenericParam> {
        generics
            .iter()
            .map(|g| HirGenericParam {
                ident: g.ident.clone(),
                bound: g.bound.clone(),
                default: g.default.clone(),
            })
            .collect()
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
                            HirAnnotationArg::KeyValue(
                                key.clone(),
                                HirAnnotationValue::IntLiteral(value.clone()),
                            )
                        }
                        AnnotationArg::KeyValue(key, AnnotationValue::Sizeof(r#type)) => {
                            HirAnnotationArg::KeyValue(
                                key.clone(),
                                HirAnnotationValue::Sizeof(r#type.clone()),
                            )
                        }
                        AnnotationArg::KeyValue(key, AnnotationValue::StrLiteral(value)) => {
                            HirAnnotationArg::KeyValue(
                                key.clone(),
                                HirAnnotationValue::StrLiteral(value.clone()),
                            )
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
        let functions = sp
            .functions
            .iter()
            .map(|f| self.lower_spec_function(f, span))
            .collect();
        let annotations = Self::lower_annotations(&sp.annotations);

        HirSpecDef {
            id,
            span,
            visibility: sp.visibility,
            name: sp.ident.clone(),
            generics,
            dependencies,
            functions,
            is_alias: sp.is_alias,
            annotations,
        }
    }

    fn lower_spec_function(&mut self, f: &SpecFunctionStmt, span: Span) -> HirSpecFunction {
        let mut params = Vec::with_capacity(f.params.len() + 1);
        if let Some(p) = self.self_param(f.self_mode, span) {
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
            is_variadic: f.is_variadic,
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
            visibility: param.visibility,
        }
    }

    fn lower_struct_def(&mut self, s: &StructStmt, span: Span) -> HirStructDef {
        let id = self.ids.next();
        let fields = s.fields.iter().map(|f| self.lower_param(f, span)).collect();
        let functions = s
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, span, true))
            .collect();

        HirStructDef {
            id,
            span,
            annotations: Self::lower_annotations(&s.annotations),
            visibility: s.visibility,
            name: s.ident.clone(),
            generics: Self::lower_generics(&s.generics),
            fields,
            functions,
            is_marker: s.is_marker,
        }
    }

    /// Same treatment as `lower_struct_def` -- member functions get their
    /// synthetic `self: *Self` inserted by `lower_function_def`.
    fn lower_union_def(&mut self, u: &UnionStmt, span: Span) -> HirUnionDef {
        let id = self.ids.next();
        let fields = u.fields.iter().map(|f| self.lower_param(f, span)).collect();
        let functions = u
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, span, true))
            .collect();

        HirUnionDef {
            id,
            span,
            annotations: Self::lower_annotations(&u.annotations),
            visibility: u.visibility,
            name: u.ident.clone(),
            generics: Self::lower_generics(&u.generics),
            fields,
            functions,
        }
    }

    /// Same treatment as `lower_struct_def` -- member functions get their
    /// synthetic `self: *Self` inserted by `lower_function_def`, exactly
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
                visibility: h.visibility,
            })
            .collect();
        let dynamic_fields = e
            .dynamic_fields
            .iter()
            .map(|f| self.lower_param(f, span))
            .collect();
        let variants = e
            .variants
            .iter()
            .map(|v| HirEnumVariant {
                id: self.ids.next(),
                span: v.span,
                name: v.ident.clone(),
                args: v.args.iter().map(|a| self.lower_expr(a)).collect(),
                fields: v
                    .fields
                    .iter()
                    .map(|f| self.lower_param(f, v.span))
                    .collect(),
            })
            .collect();
        let functions = e
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, span, true))
            .collect();

        HirEnumDef {
            id,
            span,
            annotations: Self::lower_annotations(&e.annotations),
            visibility: e.visibility,
            name: e.ident.clone(),
            generics: Self::lower_generics(&e.generics),
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
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Codeblock(block),
                }
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
                    expr: HirExpr::If(HirIf {
                        branches,
                        else_branch,
                    }),
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
                    expr: HirExpr::CompoundAssign(HirCompoundAssign {
                        target,
                        op: assign.op,
                        value,
                    }),
                }
            }
            Expression::AddressOf(addr) => {
                let base = Box::new(self.lower_expr(&addr.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::AddressOf(HirAddressOf {
                        base,
                        mutable: addr.mutable,
                    }),
                }
            }
            Expression::Reveal(reveal) => {
                let base = Box::new(self.lower_expr(&reveal.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Reveal(base),
                }
            }
            Expression::Comp(comp) => {
                let base = Box::new(self.lower_expr(&comp.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Comp(base),
                }
            }
            Expression::Negate(neg) => {
                let base = Box::new(self.lower_expr(&neg.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Negate(base),
                }
            }
            Expression::BitNot(not) => {
                let base = Box::new(self.lower_expr(&not.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::BitNot(base),
                }
            }
            Expression::Cast(cast) => {
                let base = Box::new(self.lower_expr(&cast.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Cast(HirCast {
                        target: cast.target.clone(),
                        base,
                    }),
                }
            }
            Expression::Sizeof(sizeof) => HirExprNode {
                id: self.ids.next(),
                span: node.span,
                expr: HirExpr::Sizeof(sizeof.r#type.clone()),
            },
            Expression::Increment(incr) => {
                let base = Box::new(self.lower_expr(&incr.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Increment(base),
                }
            }
            Expression::Decrement(decr) => {
                let base = Box::new(self.lower_expr(&decr.base));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Decrement(base),
                }
            }
            Expression::BinaryOp(bin) => {
                let left = Box::new(self.lower_expr(&bin.left));
                let right = Box::new(self.lower_expr(&bin.right));
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::BinaryOp(HirBinaryOp {
                        op: bin.op,
                        left,
                        right,
                    }),
                }
            }
            Expression::ArrayLiteral(lit) => {
                let elements = lit.elements.iter().map(|e| self.lower_expr(e)).collect();
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::ArrayLiteral(elements),
                }
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
                    expr: HirExpr::StructLiteral(HirStructLiteral {
                        path: lit.path.clone(),
                        fields,
                    }),
                }
            }
            Expression::Slice(s) => {
                let base = self.lower_place_chain(&s.base);
                let range = self.lower_range(&s.range);
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Slice(HirSlice { base, range }),
                }
            }
            Expression::Range(r) => {
                let range = self.lower_range(r);
                HirExprNode {
                    id: self.ids.next(),
                    span: node.span,
                    expr: HirExpr::Range(range),
                }
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
                    expr: HirExpr::Match(HirMatch {
                        scrutinee,
                        arms,
                        else_branch,
                    }),
                }
            }
            Expression::MacroInvocation(_) => unreachable!(
                "macro invocations are replaced by their expansion by \
                 omega_parser::macros::expand before lower_module runs"
            ),
        }
    }

    /// See `HirRange`'s doc comment -- shared, structural lowering for
    /// `HirSlice`, `HirPattern::Range`, and `HirExpr::Range` alike.
    fn lower_range(&mut self, range: &RangeExpr) -> HirRange {
        HirRange {
            start: range.start.as_ref().map(|e| Box::new(self.lower_expr(e))),
            end: range.end.end_expr().map(|e| Box::new(self.lower_expr(e))),
            inclusive: range.inclusive(),
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
