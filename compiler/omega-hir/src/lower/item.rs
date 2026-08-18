use super::Lowerer;
use crate::hir::{
    HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirBlock, HirConformDef, HirDeclaration,
    HirEnumDef, HirEnumVariant, HirExpr, HirExprNode, HirExternDeclaration, HirField,
    HirFunctionDef, HirGapDef, HirGapFunction, HirGenericParam, HirGlueDef, HirImport, HirItem,
    HirParam, HirPlace, HirPlaceRoot, HirPrimitiveDef, HirSpecDef, HirSpecFunction, HirStmt,
    HirStructDef, HirUnionDef, HirWalrusDeclaration,
};
use omega_parser::prelude::{
    AnnotationArg, AnnotationNode, AnnotationValue, DeclarationStmt, EnumStmt,
    ExternDeclarationStmt, FunctionDefinitionStmt, GenericParam, Ident, Item, ItemNode, Param,
    Path, SelfMode, Span, SpecFunctionStmt, SpecStmt, StructStmt, Type, UnionStmt,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FunctionKind {
    Free,
    Member,
}

impl FunctionKind {
    fn has_implicit_self(self) -> bool {
        self == Self::Member
    }
}

impl Lowerer {
    pub(super) fn lower_item(&mut self, node: &ItemNode) -> HirItem {
        match &node.item {
            Item::Declaration(decl) => HirItem::Declaration {
                decl: self.lower_declaration(decl),
                visibility: decl.visibility,
            },
            Item::DeclarationWithInit(decl, value) => HirItem::DeclarationWithInit {
                decl: self.lower_declaration(decl),
                value: self.lower_expr(value),
                visibility: decl.visibility,
            },
            Item::Walrus(w) => HirItem::Walrus {
                visibility: w.visibility,
                walrus: HirWalrusDeclaration {
                    id: self.ids.next(),
                    span: node.span,
                    ident: w.ident.clone(),
                    origin: w.origin,
                    value: self.lower_expr(&w.value),
                    mutable: w.mutable,
                    comp: w.comp,
                },
            },
            Item::ExternDeclaration(decl) => {
                HirItem::ExternDeclaration(self.lower_extern_declaration(decl, node.span))
            }
            Item::FunctionDefinition(f) => {
                HirItem::FunctionDefinition(self.lower_function_def(f, FunctionKind::Free))
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
                        span: f.signature_span,
                        name_span: f.name_span,
                        name: f.ident.clone(),
                        params: f.params.iter().map(|p| self.lower_param(p)).collect(),
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
                    .map(|f| self.lower_function_def(f, FunctionKind::Free))
                    .collect(),
            }),
            Item::Conform(conform) => HirItem::Conform(HirConformDef {
                id: self.ids.next(),
                span: node.span,
                generics: Self::lower_generics(&conform.generics),
                target: conform.target.clone(),
                spec: conform.spec.clone(),
                functions: conform
                    .functions
                    .iter()
                    .map(|f| self.lower_function_def(f, FunctionKind::Member))
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
                    .map(|f| self.lower_function_def(f, FunctionKind::Member))
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

    pub(super) fn lower_declaration(&mut self, decl: &DeclarationStmt) -> HirDeclaration {
        HirDeclaration {
            id: self.ids.next(),
            span: decl.span,
            ident: decl.ident.clone(),
            origin: decl.origin,
            r#type: decl.r#type.clone(),
            mutable: decl.mutable,
        }
    }

    pub(super) fn lower_extern_declaration(
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

    fn lower_function_def(
        &mut self,
        f: &FunctionDefinitionStmt,
        kind: FunctionKind,
    ) -> HirFunctionDef {
        let span = f.signature_span.to(f.codeblock.span);
        let mut params = self.lower_callable_params(&f.params, f.self_mode, span, kind);
        let mut body = self.lower_block(&f.codeblock);
        self.prepend_mut_self_shadow(&mut body, f.self_mode, span);

        let mut generics = Self::lower_generics(&f.generics);
        Self::desugar_spec_static_params(&mut params, &mut generics);

        HirFunctionDef {
            id: self.ids.next(),
            span,
            name_span: f.name_span,
            signature_span: f.signature_span,
            return_type_span: f.return_type_span,
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
                    bounds: vec![(**bound).clone()],
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
                for p in f.params.iter_mut() {
                    Self::replace_spec_static(&mut p.r#type, next, generics);
                }
                Self::replace_spec_static(&mut f.return_type, next, generics);
            }
            Type::Named(_) | Type::SpecObject(_, _) => {}
        }
    }

    fn lower_callable_params(
        &mut self,
        params: &[Param],
        self_mode: Option<SelfMode>,
        span: Span,
        kind: FunctionKind,
    ) -> Vec<HirParam> {
        let self_capacity = usize::from(kind.has_implicit_self() && self_mode.is_some());
        let mut lowered = Vec::with_capacity(params.len() + self_capacity);
        if kind.has_implicit_self()
            && let Some(self_param) = self.self_param(self_mode, span)
        {
            lowered.push(self_param);
        }
        lowered.extend(params.iter().map(|param| self.lower_param(param)));
        lowered
    }

    fn prepend_mut_self_shadow(
        &mut self,
        body: &mut HirBlock,
        self_mode: Option<SelfMode>,
        span: Span,
    ) {
        if self_mode == Some(SelfMode::MutValue) {
            body.stmts.insert(0, self.self_shadow_stmt(span));
        }
    }

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
            // The parser preserves the self mode but not the `self` token span,
            // so this synthetic parameter falls back to the function span.
            name_span: span,
            ident: Ident("self".to_string()),
            origin: omega_parser::prelude::Origin::default(),
            r#type,
        })
    }

    fn self_shadow_stmt(&mut self, span: Span) -> HirStmt {
        let self_ident = Ident("self".to_string());
        HirStmt::WalrusDeclaration(HirWalrusDeclaration {
            id: self.ids.next(),
            span,
            ident: self_ident.clone(),
            origin: omega_parser::prelude::Origin::default(),
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
        })
    }

    fn lower_generics(generics: &[GenericParam]) -> Vec<HirGenericParam> {
        generics
            .iter()
            .map(|g| HirGenericParam {
                ident: g.ident.clone(),
                bounds: g.bounds.clone(),
                default: g.default.clone(),
            })
            .collect()
    }

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
            .map(|f| self.lower_spec_function(f))
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

    fn lower_spec_function(&mut self, f: &SpecFunctionStmt) -> HirSpecFunction {
        let span = f
            .body
            .as_ref()
            .map_or(f.signature_span, |body| f.signature_span.to(body.span));
        let params = self.lower_callable_params(&f.params, f.self_mode, span, FunctionKind::Member);
        let mut body = f.body.as_ref().map(|block| self.lower_block(block));
        if let Some(body) = &mut body {
            self.prepend_mut_self_shadow(body, f.self_mode, span);
        }

        HirSpecFunction {
            id: self.ids.next(),
            span,
            name_span: f.name_span,
            signature_span: f.signature_span,
            return_type_span: f.return_type_span,
            name: f.ident.clone(),
            self_mode: f.self_mode,
            params,
            is_variadic: f.is_variadic,
            return_type: f.return_type.clone(),
            body,
        }
    }

    fn lower_param(&mut self, param: &Param) -> HirParam {
        HirParam {
            id: self.ids.next(),
            span: param.span,
            name_span: param.name_span,
            ident: param.ident.clone(),
            origin: param.origin,
            r#type: param.r#type.clone(),
        }
    }

    fn lower_field(&mut self, field: &DeclarationStmt) -> HirField {
        HirField {
            id: self.ids.next(),
            span: field.span,
            name_span: field.name_span,
            ident: field.ident.clone(),
            origin: field.origin,
            r#type: field.r#type.clone(),
            visibility: field.visibility,
        }
    }

    fn lower_struct_def(&mut self, s: &StructStmt, span: Span) -> HirStructDef {
        let id = self.ids.next();
        let fields = s.fields.iter().map(|f| self.lower_field(f)).collect();
        let functions = s
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, FunctionKind::Member))
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

    fn lower_union_def(&mut self, u: &UnionStmt, span: Span) -> HirUnionDef {
        let id = self.ids.next();
        let fields = u.fields.iter().map(|f| self.lower_field(f)).collect();
        let functions = u
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, FunctionKind::Member))
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

    fn lower_enum_def(&mut self, e: &EnumStmt, span: Span) -> HirEnumDef {
        let id = self.ids.next();
        let header = e
            .header
            .iter()
            .map(|h| HirField {
                id: self.ids.next(),
                span: h.span,
                name_span: h.name_span,
                ident: h.ident.clone(),
                origin: omega_parser::prelude::Origin::default(),
                r#type: h.r#type.clone(),
                visibility: h.visibility,
            })
            .collect();
        let dynamic_fields = e
            .dynamic_fields
            .iter()
            .map(|f| self.lower_field(f))
            .collect();
        let variants = e
            .variants
            .iter()
            .map(|v| HirEnumVariant {
                id: self.ids.next(),
                span: v.span,
                name: v.ident.clone(),
                args: v.args.iter().map(|a| self.lower_expr(a)).collect(),
                fields: v.fields.iter().map(|f| self.lower_field(f)).collect(),
            })
            .collect();
        let functions = e
            .functions
            .iter()
            .map(|f| self.lower_function_def(f, FunctionKind::Member))
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
}
