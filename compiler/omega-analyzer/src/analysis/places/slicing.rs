use super::*;

impl<'r> Analyzer<'r> {
    pub(crate) fn analyze_slice(
        &mut self,
        node_id: HirId,
        span: Span,
        base: &HirPlace,
        range: &HirRange,
        requested_mutable: bool,
    ) -> Option<CheckedExprNode> {
        let (mut checked_base, base_type, place_mutable) =
            self.analyze_place(node_id, span, base, None)?;
        let base_type_snapshot = base_type.clone();
        let base_lacks_length = matches!(base_type_snapshot, ResolvedType::Array(_, _));

        // The slice's *source* mutability: for inline storage
        // (`SizedArray`), whether the storage being sliced is itself
        // writable (the place's own mutability); for re-slicing an
        // existing `Slice` value, that slice's own flag -- a property of
        // the value being sliced, independent of whether the *variable*
        // holding it happens to be `mut`. `requested_mutable` (from the
        // `&`/`&mut` the user actually wrote) may never exceed this: you
        // can't get a mutable slice out of an immutable array or an
        // already-immutable slice.
        // `is_str` tracks whether the *result* should be a `Str` instead of
        // a `Slice` -- re-slicing a `*str` produces another `*str`, not a
        // `*[u8]`, even though `item_type` (used for the byte-offset math
        // both share) is `U8` either way.
        let (item_type, source_mutable, from_fat_pointer, is_str) = match base_type {
            ResolvedType::SizedArray(item_type, _) => (*item_type, place_mutable, false, false),
            ResolvedType::Slice { item, mutable } => (*item, mutable, true, false),
            ResolvedType::Str { mutable } => (ResolvedType::U8, mutable, true, true),
            ResolvedType::Array(item, mutable) => (*item, mutable, true, false),
            found => {
                self.error(node_id, span, AnalysisErrorKind::NotSliceable { found });
                return None;
            }
        };
        if requested_mutable && !source_mutable {
            if from_fat_pointer {
                self.error(node_id, span, AnalysisErrorKind::ImmutableSliceSource);
                return None;
            }
            self.require_mutable_place(node_id, span, &base.root, &checked_base, source_mutable)?;
        }

        // A `comp`-bound `*[?]T` has no established promotion story: unlike
        // `SizedArray` (promote its address) or `Slice`/`Str` (already its
        // own two leaves), a `comp` array's own `ConstValue` shape isn't
        // one either of the two branches below already knows how to
        // handle, and speculatively guessing at one risks silently
        // building the wrong leaves rather than just rejecting a genuinely
        // narrow, likely-never-hit combination outright.
        if base_lacks_length
            && matches!(
                checked_base.root,
                CheckedPlaceRoot::Variable {
                    storage: Storage::Comp,
                    ..
                }
            )
        {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::CompPointerSliceNotSupported,
            );
            return None;
        }

        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = checked_base.root
        {
            let value = self.resolve_comp_place(node_id, span, &checked_base)?;
            checked_base = if from_fat_pointer {
                let r#type = if is_str {
                    ResolvedType::Str { mutable: false }
                } else {
                    ResolvedType::Slice {
                        item: Box::new(item_type.clone()),
                        mutable: false,
                    }
                };
                CheckedPlace {
                    root: CheckedPlaceRoot::Expr(Box::new(CheckedExprNode {
                        id: self.resolver.fresh_synthetic_id(),
                        span,
                        r#type: r#type.clone(),
                        kind: CheckedExpr::Const(value),
                    })),
                    projections: vec![],
                    r#type,
                }
            } else {
                CheckedPlace {
                    root: CheckedPlaceRoot::Expr(Box::new(CheckedExprNode {
                        id: self.resolver.fresh_synthetic_id(),
                        span,
                        r#type: ResolvedType::Pointer {
                            pointee: Box::new(item_type.clone()),
                            mutable: false,
                        },
                        kind: CheckedExpr::Const(ConstValue::Ref(Box::new(value))),
                    })),
                    projections: vec![CheckedProjection::Deref {
                        r#type: base_type_snapshot.clone(),
                    }],
                    r#type: base_type_snapshot,
                }
            };
        }

        let analyze_bound = |this: &mut Self,
                             bound: Option<&HirExprNode>|
         -> Option<Option<Box<CheckedExprNode>>> {
            let Some(bound) = bound else {
                return Some(None);
            };
            let checked_bound = this.analyze_expr(bound, Some(&ResolvedType::I32))?;
            if checked_bound.r#type != ResolvedType::I32 {
                this.error(
                    bound.id,
                    bound.span,
                    AnalysisErrorKind::InvalidSliceBound {
                        r#type: checked_bound.r#type,
                    },
                );
                return None;
            }
            Some(Some(Box::new(checked_bound)))
        };

        let checked_start = analyze_bound(self, range.start.as_deref())?;
        let checked_end = match &range.end {
            HirRangeEnd::Inclusive(end) => CheckedRangeEnd::Inclusive(
                analyze_bound(self, Some(end))?.expect("range has an end"),
            ),
            HirRangeEnd::Exclusive(end) => CheckedRangeEnd::Exclusive(
                analyze_bound(self, Some(end))?.expect("range has an end"),
            ),
            HirRangeEnd::Open => CheckedRangeEnd::Open,
        };
        // A missing `start` always defaults to `0`, fine for every base
        // kind -- but a missing `end` only has something to default to
        // when `base_lacks_length` is `false` (`SizedArray`'s compile-time
        // `N`, or `Slice`/`Str`'s own runtime length leaf). `*[?]T` (`Array`)
        // has no such fallback anywhere, so `&arr[a..]` must name its own
        // end explicitly; `&arr[a..<b]`/`&arr[..<b]` are unaffected.
        if matches!(checked_end, CheckedRangeEnd::Open) && base_lacks_length {
            self.error(node_id, span, AnalysisErrorKind::MissingSliceEnd);
            return None;
        }

        let result_type = if is_str {
            ResolvedType::Str {
                mutable: requested_mutable,
            }
        } else {
            ResolvedType::Slice {
                item: Box::new(item_type.clone()),
                mutable: requested_mutable,
            }
        };
        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: result_type,
            kind: CheckedExpr::Slice(CheckedSlice {
                base: checked_base,
                item_type,
                start: checked_start,
                end: checked_end,
            }),
        })
    }

    pub(crate) fn analyze_const_slice(
        &mut self,
        node_id: HirId,
        span: Span,
        elements: &[HirExprNode],
        mutable: bool,
        expected: Option<&ResolvedType>,
    ) -> Option<CheckedExprNode> {
        if mutable {
            self.error(node_id, span, AnalysisErrorKind::ConstSliceCannotBeMutable);
            return None;
        }
        if elements.is_empty() {
            self.error(node_id, span, AnalysisErrorKind::EmptyArrayLiteral);
            return None;
        }

        let item_type = match expected {
            Some(ResolvedType::Slice {
                item,
                mutable: false,
            }) => item.as_ref().clone(),
            _ => self.analyze_expr(&elements[0], None)?.r#type.widened(),
        };

        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.const_eval_slice(element, &item_type)?);
        }

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: ResolvedType::Slice {
                item: Box::new(item_type),
                mutable: false,
            },
            kind: CheckedExpr::Const(ConstValue::Slice(values)),
        })
    }
}
