use super::*;

impl<'r> Analyzer<'r> {
    /// Whether `r#type` has a literal constant form -- the requirement on
    /// enum header fields (their values are per-variant constants); see
    /// `ConstValue`.
    pub(super) fn const_representable(&self, r#type: &ResolvedType) -> bool {
        r#type.numeric_kind(self.target.pointer_bits()).is_some()
            || matches!(r#type, ResolvedType::Bool | ResolvedType::Char)
            // String and compile-time slice constants are always immutable.
            || matches!(r#type, ResolvedType::Str { mutable: false })
            || matches!(r#type, ResolvedType::Slice { item, mutable: false } if self.const_representable(item))
            || matches!(r#type, ResolvedType::SizedArray(item, _) if self.const_representable(item))
    }

    /// Evaluates an enum variant's tag/header value: a literal (number,
    /// string, bool, or char -- optionally a negated number), checked
    /// against `expected` -- the *expected type drives* number-literal
    /// interpretation here (no `u32` suffix needed to satisfy a `u32`
    /// header field), unlike ordinary expressions, since a constant
    /// position has nothing else to infer from.
    pub(super) fn const_eval(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        let mismatch = |this: &mut Self, found: &str| {
            this.error(
                expr.id,
                expr.span,
                AnalysisErrorKind::EnumValueTypeMismatch { expected: expected.clone(), found: found.into() },
            );
            None
        };
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => {
                    self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number)
                }
                _ => {
                    self.error(expr.id, expr.span, AnalysisErrorKind::EnumValueNotConstant);
                    None
                }
            },
            HirExpr::String(s) => match expected {
                ResolvedType::Str { mutable: false } => Some(ConstValue::Str(s.0.clone())),
                _ => mismatch(self, "a string literal"),
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => mismatch(self, "a bool literal"),
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => mismatch(self, "a character literal"),
            },
            // A bare `[...]` -- a fixed-length array has no indirection of
            // its own, so it's never written with a leading `&`. Every
            // variant must supply exactly `size` elements.
            HirExpr::ArrayLiteral(elements) => match expected {
                ResolvedType::SizedArray(item, size) => {
                    if elements.len() != *size as usize {
                        return mismatch(self, &format!("an array literal with {} elements", elements.len()));
                    }
                    let mut values = Vec::with_capacity(elements.len());
                    for element in elements {
                        values.push(self.const_eval(element, item)?);
                    }
                    Some(ConstValue::Array(values))
                }
                _ => mismatch(self, "an array literal"),
            },
            // `&[...]` is the only recognized spelling for a compile-time
            // slice -- a bare `[...]` is never treated as one, to avoid
            // confusion with an ordinary array. Recurses through
            // `const_eval` itself, so nesting falls out for free.
            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                if *mutable {
                    self.error(expr.id, expr.span, AnalysisErrorKind::ConstSliceCannotBeMutable);
                    return None;
                }
                match &base.expr {
                    HirExpr::ArrayLiteral(elements) => match expected {
                        ResolvedType::Slice { item, mutable: false } => {
                            let mut values = Vec::with_capacity(elements.len());
                            for element in elements {
                                values.push(self.const_eval(element, item)?);
                            }
                            Some(ConstValue::Slice(values))
                        }
                        _ => mismatch(self, "an array literal"),
                    },
                    _ => {
                        self.error(expr.id, expr.span, AnalysisErrorKind::EnumValueNotConstant);
                        None
                    }
                }
            }
            // Not a recognized literal shape -- fall back to the general
            // `comp` evaluator: an enum header value is inherently
            // compile-time-only, so anything the interpreter can resolve
            // (arithmetic, a call, a `comp`-built struct) is as legitimate
            // as a bare literal. The literal arms above keep their own,
            // more specific error messages rather than routing through here.
            _ => match self.analyze_expr(expr, Some(expected)) {
                Some(checked) if checked.r#type == *expected => self.eval_comp(expr.id, &checked),
                Some(checked) => mismatch(self, &format!("a value of type `{}`", checked.r#type)),
                None => None,
            },
        }
    }

    /// The number-literal side of `const_eval`: parses and range-checks `n`
    /// against `expected` (which must be numeric), honoring an optional
    /// leading negation.
    pub(super) fn const_number(
        &mut self,
        node_id: HirId,
        span: Span,
        n: &NumberExpr,
        expected: &ResolvedType,
        negated: bool,
    ) -> Option<NumberValue> {
        let mismatch = |this: &mut Self, found: String| {
            this.error(node_id, span, AnalysisErrorKind::EnumValueTypeMismatch { expected: expected.clone(), found });
            None
        };
        let Some(kind) = expected.numeric_kind(self.target.pointer_bits()) else {
            return mismatch(self, "a number literal".into());
        };

        // An explicit suffix must agree with the field's declared type --
        // there are no implicit conversions to paper over a disagreement.
        if let Some(suffix) = &n.explicit_type {
            let suffixed = self.context.resolve_type(
                Type::Named(suffix.clone().into()),
                &mut *self.resolver,
                &self.module_path,
                true,
                !self.reveal_stack.is_empty(),
            );
            match suffixed {
                Ok(t) if t == *expected => {}
                Ok(t) => return mismatch(self, format!("a `{t}` literal")),
                Err(_) => {
                    self.error(node_id, span, AnalysisErrorKind::InvalidNumberType(suffix.clone()));
                    return None;
                }
            }
        }

        let is_float = matches!(kind, NumericKind::Float(_));
        if n.fractional_part.is_some() && !is_float {
            return mismatch(self, "a fractional number literal".into());
        }
        if negated && matches!(kind, NumericKind::Unsigned(_)) {
            return mismatch(self, "a negative number literal".into());
        }

        let literal_text = || {
            let digits = match &n.fractional_part {
                Some(frac) => format!("{}.{}", n.integer_part, frac),
                None => n.integer_part.clone(),
            };
            if negated { format!("-{digits}") } else { digits }
        };
        let out_of_range = |this: &mut Self| {
            this.error(
                node_id,
                span,
                AnalysisErrorKind::NumberLiteralOutOfRange { literal: literal_text(), r#type: expected.clone() },
            );
            None
        };

        match kind {
            NumericKind::Float(width) => {
                let text = format!("{}.{}", n.integer_part, n.fractional_part.as_deref().unwrap_or("0"));
                let Ok(parsed) = text.parse::<f64>() else {
                    return out_of_range(self);
                };
                if width == 32 && parsed.is_finite() && (parsed as f32).is_infinite() {
                    return out_of_range(self);
                }
                Some(NumberValue::Float(if negated { -parsed } else { parsed }))
            }
            NumericKind::Signed(width) => {
                let Ok(parsed) = u64::from_str_radix(&n.integer_part, n.base.radix()) else {
                    return out_of_range(self);
                };
                // One extra magnitude on the negative side: |i16::MIN| is
                // 32768, one past i16::MAX.
                let positive_max = if width == 64 { i64::MAX as u64 } else { (1u64 << (width - 1)) - 1 };
                let max = if negated { positive_max + 1 } else { positive_max };
                if parsed > max {
                    return out_of_range(self);
                }
                let value = if negated { (-(parsed as i128)) as i64 } else { parsed as i64 };
                Some(NumberValue::Signed(value))
            }
            NumericKind::Unsigned(width) => {
                let Ok(parsed) = u64::from_str_radix(&n.integer_part, n.base.radix()) else {
                    return out_of_range(self);
                };
                let max = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
                if parsed > max {
                    return out_of_range(self);
                }
                Some(NumberValue::Unsigned(parsed))
            }
        }
    }

    /// `analyze_const_slice`'s per-element evaluator -- the `&[...]`
    /// sibling of `const_eval`, kept separate because `const_eval`'s
    /// fallback errors are worded specifically for enum header values,
    /// which would be confusing for `a := &[1, f()];`. Otherwise identical,
    /// sharing `const_number` for parsing/range-checking.
    pub(super) fn const_eval_slice(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        let mismatch = |this: &mut Self, found: &str| {
            this.error(
                expr.id,
                expr.span,
                AnalysisErrorKind::ConstSliceElementTypeMismatch { expected: expected.clone(), found: found.into() },
            );
            None
        };
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => {
                    self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number)
                }
                _ => {
                    self.error(expr.id, expr.span, AnalysisErrorKind::ConstSliceElementNotConstant);
                    None
                }
            },
            HirExpr::String(s) => match expected {
                ResolvedType::Str { mutable: false } => Some(ConstValue::Str(s.0.clone())),
                _ => mismatch(self, "a string literal"),
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => mismatch(self, "a bool literal"),
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => mismatch(self, "a character literal"),
            },
            // A bare `[...]` -- a fixed-length array element (e.g. a slice
            // of fixed-size arrays, `*[[i32; 2]]`) has no indirection of its
            // own, so it's written the same way it would be as a bare enum
            // header value: no `&`, matching `const_eval`'s identical case.
            HirExpr::ArrayLiteral(elements) => match expected {
                ResolvedType::SizedArray(item, size) => {
                    if elements.len() != *size as usize {
                        return mismatch(self, &format!("an array literal with {} elements", elements.len()));
                    }
                    let mut values = Vec::with_capacity(elements.len());
                    for element in elements {
                        values.push(self.const_eval_slice(element, item)?);
                    }
                    Some(ConstValue::Array(values))
                }
                _ => mismatch(self, "an array literal"),
            },
            // `&[...]` is the only recognized spelling for a nested
            // compile-time slice -- a bare `[...]` is never treated as one,
            // even here, to avoid confusing it with an ordinary array.
            // `&mut [...]` still isn't allowed, even nested.
            HirExpr::AddressOf(HirAddressOf { base, mutable }) => {
                if *mutable {
                    self.error(expr.id, expr.span, AnalysisErrorKind::ConstSliceCannotBeMutable);
                    return None;
                }
                match &base.expr {
                    HirExpr::ArrayLiteral(nested) => match expected {
                        ResolvedType::Slice { item, mutable: false } => {
                            let mut values = Vec::with_capacity(nested.len());
                            for element in nested {
                                values.push(self.const_eval_slice(element, item)?);
                            }
                            Some(ConstValue::Slice(values))
                        }
                        _ => mismatch(self, "an array literal"),
                    },
                    _ => {
                        self.error(expr.id, expr.span, AnalysisErrorKind::ConstSliceElementNotConstant);
                        None
                    }
                }
            }
            // See `const_eval`'s identical fallback -- a `&[...]` compile-
            // time slice's elements are just as inherently compile-time-
            // only as an enum header's, so anything the interpreter can
            // resolve is as legitimate an element as a bare literal.
            _ => match self.analyze_expr(expr, Some(expected)) {
                Some(checked) if checked.r#type == *expected => self.eval_comp(expr.id, &checked),
                Some(checked) => mismatch(self, &format!("a value of type `{}`", checked.r#type)),
                None => None,
            },
        }
    }
}
