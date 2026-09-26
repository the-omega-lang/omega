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
    /// Rewrites every hole in `annotation`. `Ok(None)` means it has none;
    /// `Err` means a hole sits where nothing is inferred, which is reported.
    pub(crate) fn rewrite_annotation_holes(
        &mut self,
        node_id: HirId,
        span: Span,
        annotation: &Type,
    ) -> Result<Option<HoledAnnotation>, ()> {
        let mut holes = Vec::new();
        let Ok(rewritten) = self.rewrite_type_holes(node_id, span, annotation, &mut holes) else {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::UnresolvedType(TypeResolutionError::InferenceHoleNotAllowed),
            );
            return Err(());
        };
        Ok((!holes.is_empty()).then(|| HoledAnnotation {
            written: annotation.clone(),
            rewritten,
            holes,
        }))
    }

    fn rewrite_type_holes(
        &mut self,
        node_id: HirId,
        span: Span,
        ty: &Type,
        holes: &mut Vec<HirGenericParam>,
    ) -> Result<Type, ()> {
        Ok(match ty {
            Type::Infer => {
                Type::Named(fresh_hole(holes, GenericParamKind::Type { bounds: vec![] }).into())
            }
            Type::Pointer(inner, mutable) => Type::Pointer(
                Box::new(self.rewrite_type_holes(node_id, span, inner, holes)?),
                *mutable,
            ),
            Type::InferredArray(inner) => Type::InferredArray(Box::new(
                self.rewrite_type_holes(node_id, span, inner, holes)?,
            )),
            Type::UnknownSizeArray(inner) => Type::UnknownSizeArray(Box::new(
                self.rewrite_type_holes(node_id, span, inner, holes)?,
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
                    Box::new(self.rewrite_type_holes(node_id, span, inner, holes)?),
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
                            GenericArg::Type(self.rewrite_type_holes(node_id, span, inner, holes)?)
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
                            r#type: self.rewrite_type_holes(node_id, span, &param.r#type, holes)?,
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
                )?),
                ..f.clone()
            }),
            // Nothing is inferred inside a spec reference, and an anonymous
            // enum's members lose their written positions to canonical
            // ordering.
            Type::SpecStatic(_) | Type::AnonymousEnum(_) if contains_hole(ty) => return Err(()),
            Type::Named(_) | Type::SpecStatic(_) | Type::AnonymousEnum(_) => ty.clone(),
        })
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
        let pattern = self.annotation_type_pattern(
            decl_id,
            decl_span,
            &annotation.rewritten,
            &annotation.holes,
        )?;
        let checked = self.analyze_expr(value, Expected::Pattern(&pattern))?;
        let Some(solved) = read_back_holes(annotation, &pattern, &checked.r#type) else {
            self.error(
                decl_id,
                decl_span,
                AnalysisErrorKind::UninferredHole {
                    annotation: crate::error::raw_type_display(&annotation.written),
                },
            );
            return None;
        };
        let resolved = self.resolve_solved_annotation(decl_id, decl_span, annotation, &solved)?;
        Some((resolved, checked))
    }

    /// The annotation resolved as if every hole had been written as its
    /// solution.
    pub(crate) fn resolve_solved_annotation(
        &mut self,
        id: HirId,
        span: Span,
        annotation: &HoledAnnotation,
        solved: &[ResolvedGenericArg],
    ) -> Option<ResolvedType> {
        let substitution =
            GenericSubstitution::zip(annotation.holes.iter().map(|hole| &hole.ident), solved);
        self.with_substitution(&substitution, |this| {
            this.resolve_type_or_error(id, span, &annotation.rewritten, true)
        })
    }
}

/// Each hole of `annotation` taken from `found`. `None` when `found` leaves a
/// hole open.
pub(crate) fn read_back_holes(
    annotation: &HoledAnnotation,
    pattern: &TypePattern,
    found: &ResolvedType,
) -> Option<Vec<ResolvedGenericArg>> {
    let mut bindings = vec![None; annotation.holes.len()];
    pattern.infer(found, &mut bindings);
    bindings.into_iter().collect()
}

fn contains_hole(ty: &Type) -> bool {
    match ty {
        Type::Infer => true,
        Type::Named(_) => false,
        Type::Pointer(inner, _) | Type::InferredArray(inner) | Type::UnknownSizeArray(inner) => {
            contains_hole(inner)
        }
        Type::SizedArray(inner, length) => {
            matches!(length, ArrayLength::Infer) || contains_hole(inner)
        }
        Type::Generic(_, args) => args.iter().any(|arg| match arg {
            GenericArg::Infer => true,
            GenericArg::Type(inner) => contains_hole(inner),
            GenericArg::Value(_) => false,
        }),
        Type::Function(f) => {
            f.params.iter().any(|param| contains_hole(&param.r#type))
                || contains_hole(&f.return_type)
        }
        Type::SpecStatic(members) | Type::AnonymousEnum(members) => {
            members.iter().any(contains_hole)
        }
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
