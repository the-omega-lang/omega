use super::*;

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_range_value(
        &mut self,
        id: HirId,
        span: Span,
        range: &HirRange,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        // Each written bound is analyzed exactly once. The end adapts to the
        // start's type when there is one, so `lo..=255` gives the literal
        // `lo`'s type rather than letting it default to `i32` independently.
        let checked_start = match &range.start {
            Some(expr) => Some(self.analyze_expr(expr, Expected::None)?),
            None => None,
        };
        let checked_end = match (range.end.expr(), &checked_start) {
            (Some(expr), Some(start)) => {
                Some(self.analyze_expr(expr, Expected::Exact(&start.r#type))?)
            }
            (Some(expr), None) => Some(self.analyze_expr(expr, Expected::None)?),
            (None, _) => None,
        };

        let element = match (&checked_start, &checked_end) {
            (Some(start), _) => start.r#type.clone(),
            (None, Some(end)) => end.r#type.clone(),
            (None, None) => match Self::expected_range_element(expected) {
                Some(element) => element,
                None => {
                    self.error(id, span, AnalysisErrorKind::RangeNotAllowedHere);
                    return None;
                }
            },
        };

        let start = match checked_start {
            Some(value) => self.coerce_to_expected(Some(&element), value),
            None => self.synthesize_bounded_call(id, span, &element, "min")?,
        };
        let end = match checked_end {
            Some(value) => self.coerce_to_expected(Some(&element), value),
            None => self.synthesize_bounded_call(id, span, &element, "max")?,
        };

        let nominal = self.core_range(id, span, element)?;
        // Field indices, not names: `core_range` checked that `start`, `end`,
        // `inclusive` are declared in exactly this order.
        Some(CheckedExprNode {
            id,
            span,
            r#type: ResolvedType::Struct(nominal),
            kind: CheckedExpr::StructLiteral(CheckedStructLiteral {
                fields: vec![
                    CheckedStructLiteralField {
                        field_index: 0,
                        value: start,
                    },
                    CheckedStructLiteralField {
                        field_index: 1,
                        value: end,
                    },
                    CheckedStructLiteralField {
                        field_index: 2,
                        value: CheckedExprNode {
                            id: self.resolver.fresh_synthetic_id(),
                            span,
                            r#type: ResolvedType::Bool,
                            kind: CheckedExpr::Bool(range.inclusive()),
                        },
                    },
                ],
            }),
        })
    }

    fn expected_range_element(expected: Option<&ResolvedType>) -> Option<ResolvedType> {
        let ResolvedType::Struct(nominal) = expected? else {
            return None;
        };
        let definition = &**nominal;
        let is_core_range =
            crate::core_contract::is_range(&definition.module_path, &definition.name);
        is_core_range
            .then(|| {
                definition
                    .generic_args
                    .first()
                    .and_then(|arg| arg.as_type().cloned())
            })
            .flatten()
    }

    fn synthesize_bounded_call(
        &mut self,
        id: HirId,
        span: Span,
        target: &ResolvedType,
        name: &str,
    ) -> Option<CheckedExprNode> {
        let spec = self.core_spec(
            id,
            span,
            crate::core_contract::CoreFeature::Ranges,
            &crate::core_contract::bounded_path(),
            crate::core_contract::check_bounded,
        )?;
        let Some(conform) = self
            .resolver
            .conformance_for(target, &spec, &[])
            .ok()
            .flatten()
        else {
            self.error(
                id,
                span,
                AnalysisErrorKind::RangeNeedsBounded {
                    r#type: target.clone(),
                },
            );
            return None;
        };
        let method = conform
            .methods
            .into_iter()
            .find(|(method_name, method)| {
                method_name.as_ref() == name && method.fn_type.self_mode.is_none()
            })
            .map(|(_, method)| method)
            .expect("a conformance provides every requirement 'check_bounded' accepted");
        let fn_type = method.fn_type.clone();
        let function = ResolvedType::Function(fn_type.clone());
        let call_id = self.resolver.fresh_synthetic_id();
        let callee_id = self.resolver.fresh_synthetic_id();
        Some(CheckedExprNode {
            id: call_id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(CheckedExprNode {
                    id: callee_id,
                    span,
                    r#type: function.clone(),
                    kind: CheckedExpr::Place(CheckedPlace {
                        root: CheckedPlaceRoot::Variable {
                            decl_id: method.decl_id,
                            storage: Storage::Function,
                            r#type: function.clone(),
                        },
                        projections: vec![],
                        r#type: function,
                    }),
                }),
                fn_type,
                args: vec![],
            }),
        })
    }
}
