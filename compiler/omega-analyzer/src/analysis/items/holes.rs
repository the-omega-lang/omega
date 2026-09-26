use super::*;
use omega_parser::prelude::{FunctionType, FunctionTypeParam, GenericParamKind};

/// A binding annotation whose `_` holes were each replaced by a fresh
/// synthesized generic parameter, so the ordinary generic inference engine
/// can solve them.
pub(crate) struct HoledAnnotation {
    /// As written, for diagnostics: synthesized hole names never reach
    /// user-facing text.
    pub(crate) written: Type,
    pub(crate) rewritten: Type,
    pub(crate) holes: Vec<HirGenericParam>,
}

impl<'r> Analyzer<'r> {
    /// Rewrites every hole in `annotation`. `None` means it has none.
    pub(crate) fn rewrite_annotation_holes(
        &mut self,
        node_id: HirId,
        span: Span,
        annotation: &Type,
    ) -> Option<HoledAnnotation> {
        let mut holes = Vec::new();
        let rewritten = self.rewrite_type_holes(node_id, span, annotation, &mut holes);
        (!holes.is_empty()).then(|| HoledAnnotation {
            written: annotation.clone(),
            rewritten,
            holes,
        })
    }

    fn rewrite_type_holes(
        &mut self,
        node_id: HirId,
        span: Span,
        ty: &Type,
        holes: &mut Vec<HirGenericParam>,
    ) -> Type {
        match ty {
            Type::Infer => {
                Type::Named(fresh_hole(holes, GenericParamKind::Type { bounds: vec![] }).into())
            }
            Type::Pointer(inner, mutable) => Type::Pointer(
                Box::new(self.rewrite_type_holes(node_id, span, inner, holes)),
                *mutable,
            ),
            Type::InferredArray(inner) => Type::InferredArray(Box::new(
                self.rewrite_type_holes(node_id, span, inner, holes),
            )),
            Type::UnknownSizeArray(inner) => Type::UnknownSizeArray(Box::new(
                self.rewrite_type_holes(node_id, span, inner, holes),
            )),
            Type::SizedArray(inner, length) => {
                let length = match length {
                    ArrayLength::Infer => ArrayLength::Path(
                        fresh_hole(
                            holes,
                            GenericParamKind::Comp {
                                value_type: Type::Named(Ident("usize".to_string()).into()),
                            },
                        )
                        .into(),
                    ),
                    length => length.clone(),
                };
                Type::SizedArray(
                    Box::new(self.rewrite_type_holes(node_id, span, inner, holes)),
                    length,
                )
            }
            Type::Generic(path, args) => {
                let declared = if args.contains(&GenericArg::Infer) {
                    self.declared_generic_params(node_id, span, path)
                } else {
                    Vec::new()
                };
                let mut rewritten = Vec::with_capacity(args.len());
                for (position, arg) in args.iter().enumerate() {
                    rewritten.push(match arg {
                        GenericArg::Infer => {
                            let kind = match declared.get(position) {
                                Some(param) if param.is_comp() => param.kind.clone(),
                                _ => GenericParamKind::Type { bounds: vec![] },
                            };
                            GenericArg::Type(Type::Named(fresh_hole(holes, kind).into()))
                        }
                        GenericArg::Type(inner) => {
                            GenericArg::Type(self.rewrite_type_holes(node_id, span, inner, holes))
                        }
                        GenericArg::Value(_) => arg.clone(),
                    });
                }
                Type::Generic(path.clone(), rewritten)
            }
            Type::Function(f) => Type::Function(FunctionType {
                params: {
                    let mut params = Vec::with_capacity(f.params.len());
                    for param in &f.params {
                        params.push(FunctionTypeParam {
                            r#type: self.rewrite_type_holes(node_id, span, &param.r#type, holes),
                            ..param.clone()
                        });
                    }
                    params
                },
                return_type: Box::new(self.rewrite_type_holes(
                    node_id,
                    span,
                    &f.return_type,
                    holes,
                )),
                ..f.clone()
            }),
            // Nothing is inferred inside a spec reference, and an anonymous
            // enum's members lose their written positions to canonical
            // ordering; a hole in either is left for ordinary resolution to
            // reject.
            Type::Named(_) | Type::SpecStatic(_) | Type::AnonymousEnum(_) => ty.clone(),
        }
    }

    /// The generic parameters of the item a written generic type names, found
    /// through the same identity the annotation's pattern is built from. A
    /// hole's kind follows the parameter it lands on; without one it is a type.
    fn declared_generic_params(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &Path,
    ) -> Vec<HirGenericParam> {
        self.without_diagnostics(|this| {
            let absolute = this.pattern_item_path(node_id, span, path)?;
            this.resolver.item_generic_params(&absolute).ok().flatten()
        })
        .unwrap_or_default()
    }

    /// Checks an initializer against an annotation with holes. The annotation
    /// flows into the initializer as a pattern -- its known parts steer
    /// inference exactly as a complete annotation would -- and each hole is
    /// then read back from the checked type. The initializer is analyzed once.
    pub(super) fn check_holed_initializer(
        &mut self,
        decl_id: HirId,
        decl_span: Span,
        annotation: &HoledAnnotation,
        value: &HirExprNode,
    ) -> Option<(ResolvedType, CheckedExprNode)> {
        let pattern = self.overload_type_pattern(
            decl_id,
            decl_span,
            &annotation.rewritten,
            &annotation.holes,
        )?;
        let checked = self.analyze_expr(value, Expected::Pattern(&pattern))?;
        let resolved =
            self.solve_holes_from(decl_id, decl_span, annotation, &pattern, &checked.r#type);
        let Some(resolved) = resolved else {
            self.error(
                decl_id,
                decl_span,
                AnalysisErrorKind::UninferredHole {
                    annotation: crate::error::raw_type_display(&annotation.written),
                },
            );
            return None;
        };
        Some((resolved, checked))
    }

    /// The annotation with every hole taken from `found`, resolved as if it
    /// had been written that way. `None` when `found` leaves a hole open.
    pub(crate) fn solve_holes_from(
        &mut self,
        id: HirId,
        span: Span,
        annotation: &HoledAnnotation,
        pattern: &TypePattern,
        found: &ResolvedType,
    ) -> Option<ResolvedType> {
        let mut bindings = vec![None; annotation.holes.len()];
        pattern.infer(found, &mut bindings);
        let solved: Vec<ResolvedGenericArg> = bindings.into_iter().collect::<Option<_>>()?;
        let substitution =
            GenericSubstitution::zip(annotation.holes.iter().map(|hole| &hole.ident), &solved);
        self.with_substitution(&substitution, |this| {
            this.resolve_type_or_error(id, span, &annotation.rewritten, true)
        })
    }
}

fn fresh_hole(holes: &mut Vec<HirGenericParam>, kind: GenericParamKind) -> Ident {
    let ident = Ident(format!("$Hole{}", holes.len()));
    holes.push(HirGenericParam {
        ident: ident.clone(),
        kind,
        default: None,
    });
    ident
}
