use super::Lowerer;
use crate::hir::{
    HirAlias, HirAnnotation, HirAnnotationArg, HirAnnotationValue, HirBlock, HirConformDef,
    HirDeclaration, HirEnumDef, HirEnumVariant, HirExpr, HirExprNode, HirField, HirForeignBinding,
    HirForeignFunction, HirFunctionDef, HirGapDef, HirGapFunction, HirGenericParam, HirGlueDef,
    HirImport, HirItem, HirParam, HirPlace, HirPlaceRoot, HirPrimitiveDef, HirSpecDef,
    HirSpecFunction, HirStmt, HirStructDef, HirUnionDef, HirWalrusDeclaration,
};
use omega_parser::prelude::{
    AnnotationArg, AnnotationNode, AnnotationValue, DeclarationStmt, EnumStmt, ForeignBindingItem,
    ForeignBlockEntry, ForeignBlockItem, ForeignFunctionItem, FunctionDefinitionStmt, GenericParam,
    Ident, Item, ItemNode, Param, Path, SelfMode, Span, SpecFunctionStmt, SpecStmt, StructStmt,
    Type, UnionStmt,
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
    pub(super) fn lower_item(&mut self, node: &ItemNode) -> Vec<HirItem> {
        let item = match &node.item {
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
            Item::ForeignBinding(binding) => {
                HirItem::ForeignBinding(self.lower_foreign_binding(binding, node.span))
            }
            Item::ForeignFunction(f) => {
                HirItem::ForeignFunction(self.lower_foreign_function(f, node.span))
            }
            // Syntactic grouping only: the block's convention is applied to
            // each direct function entry here, and the block itself has no
            // HIR representation past this point.
            Item::ForeignBlock(block) => {
                return self.lower_foreign_block(block, node.span);
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
                        visibility: f.visibility,
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
            Item::Alias(alias) => HirItem::Alias(HirAlias {
                id: self.ids.next(),
                span: node.span,
                name_span: alias.name_span,
                target_span: alias.target_span,
                name: alias.ident.clone(),
                visibility: alias.visibility,
                explicit_hidden_span: alias.explicit_hidden_span,
                generics: Self::lower_generics(&alias.generics),
                target: alias.target.clone(),
            }),
            Item::Import(import) => {
                let annotations = Self::lower_annotations(&import.annotations);
                return import
                    .leaves()
                    .into_iter()
                    .map(|leaf| {
                        HirItem::Import(HirImport {
                            id: self.ids.next(),
                            span: leaf.span,
                            annotations: annotations.clone(),
                            reveal: leaf.reveal,
                            name: leaf.name,
                            path: leaf.path,
                        })
                    })
                    .collect();
            }
            Item::MacroDefinition(_) | Item::MacroInvocation(_) => {
                unreachable!(
                    "macros are fully expanded (definitions removed, invocations replaced by \
                     their expansion) by omega_parser::macros::expand before lower_module runs"
                )
            }
        };
        vec![item]
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

    fn lower_foreign_binding(
        &mut self,
        binding: &ForeignBindingItem,
        span: Span,
    ) -> HirForeignBinding {
        HirForeignBinding {
            id: self.ids.next(),
            span,
            annotations: Self::lower_annotations(&binding.annotations),
            ident: binding.ident.clone(),
            name_span: binding.name_span,
            r#type: binding.r#type.clone(),
            visibility: binding.visibility,
            explicit_hidden_span: binding.explicit_hidden_span,
        }
    }

    fn lower_foreign_function(
        &mut self,
        f: &ForeignFunctionItem,
        span: Span,
    ) -> HirForeignFunction {
        let params = f.params.iter().map(|p| self.lower_param(p)).collect();
        let body = f.body.as_ref().map(|block| self.lower_block(block));
        HirForeignFunction {
            id: self.ids.next(),
            span,
            name_span: f.name_span,
            signature_span: f.signature_span,
            return_type_span: f.return_type_span,
            annotations: Self::lower_annotations(&f.annotations),
            visibility: f.visibility,
            explicit_hidden_span: f.explicit_hidden_span,
            convention: f.convention.clone(),
            name: f.ident.clone(),
            generics: Self::lower_generics(&f.generics),
            params,
            is_variadic: f.is_variadic,
            return_type: f.return_type.clone(),
            body,
        }
    }

    fn lower_foreign_block(&mut self, block: &ForeignBlockItem, span: Span) -> Vec<HirItem> {
        block
            .entries
            .iter()
            .map(|entry| match entry {
                ForeignBlockEntry::Binding(binding) => {
                    HirItem::ForeignBinding(self.lower_foreign_binding(binding, span))
                }
                ForeignBlockEntry::Function(f) => {
                    HirItem::ForeignFunction(self.lower_foreign_function(f, span))
                }
            })
            .collect()
    }

    fn lower_function_def(
        &mut self,
        f: &FunctionDefinitionStmt,
        kind: FunctionKind,
    ) -> HirFunctionDef {
        let span = f.signature_span.to(f.codeblock.span);
        let params = self.lower_callable_params(&f.params, f.self_mode, span, kind);
        let mut body = self.lower_block(&f.codeblock);
        // A naked function's body must stay exactly the user-authored `asm`
        // statement for later naked-body validation; the synthetic `mut self`
        // shadow is meaningless for an ABI-only receiver anyway.
        let is_naked = f.annotations.iter().any(|a| a.name.as_ref() == "naked");
        if !is_naked {
            self.prepend_mut_self_shadow(&mut body, f.self_mode, span);
        }

        let generics = Self::lower_generics(&f.generics);

        HirFunctionDef {
            id: self.ids.next(),
            span,
            name_span: f.name_span,
            signature_span: f.signature_span,
            return_type_span: f.return_type_span,
            annotations: Self::lower_annotations(&f.annotations),
            visibility: f.visibility,
            explicit_hidden_span: f.explicit_hidden_span,
            name: f.ident.clone(),
            generics,
            self_mode: f.self_mode,
            params,
            return_type: f.return_type.clone(),
            body,
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
                origin: omega_parser::prelude::Origin::default(),
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
            explicit_hidden_span: sp.explicit_hidden_span,
            name: sp.ident.clone(),
            generics,
            functions,
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
            visibility: f.visibility,
            explicit_hidden_span: f.explicit_hidden_span,
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
            explicit_hidden_span: field.explicit_hidden_span,
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
            explicit_hidden_span: s.explicit_hidden_span,
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
            explicit_hidden_span: u.explicit_hidden_span,
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
                explicit_hidden_span: h.explicit_hidden_span,
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
            explicit_hidden_span: e.explicit_hidden_span,
            name: e.ident.clone(),
            generics: Self::lower_generics(&e.generics),
            header,
            dynamic_fields,
            variants,
            functions,
        }
    }
}
