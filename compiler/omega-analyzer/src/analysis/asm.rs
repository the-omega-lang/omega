use super::*;

/// One `$name`/`$N` operand-binding reference found in a raw asm body, with
/// the span it occupies in the *original source* (bodies are captured
/// verbatim by the lexer, so byte offsets inside `body` line up exactly with
/// `body_span.start`-relative offsets into the source file).
enum AsmBindingRef {
    Named { text: String, span: Span },
    Positional { index: u32, span: Span },
}

/// Scans a raw asm body for `$name`/`$N` operand bindings, recognizing `$$`
/// (a literal `$`) first so it is never misread as a binding. Anything else
/// starting with `$` (for example a backend's own `${...}` template syntax)
/// is left untouched -- Omega only ever claims these two binding shapes.
fn scan_asm_bindings(body: &str, body_start: usize) -> Vec<AsmBindingRef> {
    let bytes = body.as_bytes();
    let mut refs = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        if body[i..].starts_with("$$") {
            i += 2;
            continue;
        }
        let rest = &body[i + 1..];
        if let Some(name_len) = ident_prefix_len(rest) {
            let text = format!("${}", &rest[..name_len]);
            refs.push(AsmBindingRef::Named {
                text,
                span: Span::new(body_start + i, body_start + i + 1 + name_len),
            });
            i += 1 + name_len;
        } else if let Some(digit_len) = digit_prefix_len(rest) {
            let index: u32 = rest[..digit_len].parse().unwrap_or(u32::MAX);
            refs.push(AsmBindingRef::Positional {
                index,
                span: Span::new(body_start + i, body_start + i + 1 + digit_len),
            });
            i += 1 + digit_len;
        } else {
            i += 1;
        }
    }
    refs
}

fn ident_prefix_len(s: &str) -> Option<usize> {
    let mut len = 0;
    for c in s.chars() {
        if c.is_ascii_alphabetic() || c == '_' {
            len += c.len_utf8();
        } else if len > 0 && c.is_ascii_digit() {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    (len > 0).then_some(len)
}

fn digit_prefix_len(s: &str) -> Option<usize> {
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    (len > 0).then_some(len)
}

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_inline_asm(&mut self, asm: &HirInlineAsm) -> Option<Vec<CheckedStmt>> {
        let mut checked_descriptors = Vec::with_capacity(asm.descriptors.len());
        let mut ok = true;
        for descriptor in &asm.descriptors {
            match self.analyze_asm_descriptor(descriptor) {
                Some(checked) => checked_descriptors.push(checked),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        if !self.validate_asm_bindings(asm.id, &asm.body, asm.body_span, &checked_descriptors) {
            return None;
        }
        Some(vec![CheckedStmt::InlineAsm(CheckedInlineAsm {
            id: asm.id,
            span: asm.span,
            descriptors: checked_descriptors,
            body: asm.body.clone(),
            body_span: asm.body_span,
        })])
    }

    fn analyze_asm_descriptor(
        &mut self,
        descriptor: &HirAsmDescriptor,
    ) -> Option<CheckedAsmDescriptor> {
        match &descriptor.kind {
            HirAsmDescriptorKind::Reg { expr, physical } => {
                if self.in_naked_asm {
                    self.error(
                        descriptor.id,
                        descriptor.span,
                        AnalysisErrorKind::AsmRegInNakedFunction,
                    );
                    return None;
                }
                let checked_expr = self.analyze_expr(expr, None)?;
                if !Self::is_one_register_type(&checked_expr.r#type) {
                    self.error(
                        descriptor.id,
                        checked_expr.span,
                        AnalysisErrorKind::AsmRegNotOneRegisterOperand {
                            r#type: checked_expr.r#type.clone(),
                        },
                    );
                    return None;
                }
                let binding_name = Self::infer_reg_binding_name(expr);
                Some(CheckedAsmDescriptor {
                    span: descriptor.span,
                    binding_name,
                    kind: CheckedAsmDescriptorKind::Reg {
                        expr: checked_expr,
                        physical: physical.clone(),
                    },
                })
            }
            HirAsmDescriptorKind::Const { name, origin } => {
                // Reuse ordinary identifier-place resolution so `const(NAME)`
                // goes through the same `Storage::Comp` -> `CheckedExpr::Const`
                // path a plain `comp` identifier expression would.
                let synthetic = HirExprNode {
                    id: descriptor.id,
                    span: descriptor.span,
                    expr: HirExpr::Place(HirPlace {
                        root: HirPlaceRoot::Path(ExprPath {
                            path: Path {
                                head: name.clone(),
                                tail: Vec::new(),
                                origin: *origin,
                            },
                            generic_args: Vec::new(),
                            args_at: 0,
                            qualified_spec: None,
                        }),
                        projections: Vec::new(),
                    }),
                };
                let checked = self.analyze_expr(&synthetic, None)?;
                let CheckedExpr::Const(value) = checked.kind else {
                    self.error(
                        descriptor.id,
                        descriptor.span,
                        AnalysisErrorKind::AsmConstNotComp,
                    );
                    return None;
                };
                let Some(text) = Self::render_asm_const(&value) else {
                    self.error(
                        descriptor.id,
                        descriptor.span,
                        AnalysisErrorKind::AsmConstUnsupportedShape,
                    );
                    return None;
                };
                Some(CheckedAsmDescriptor {
                    span: descriptor.span,
                    binding_name: Some(name.clone()),
                    kind: CheckedAsmDescriptorKind::Const { text },
                })
            }
            HirAsmDescriptorKind::Clobber { register } => Some(CheckedAsmDescriptor {
                span: descriptor.span,
                binding_name: None,
                kind: CheckedAsmDescriptorKind::Clobber {
                    register: register.clone(),
                },
            }),
        }
    }

    fn is_one_register_type(r#type: &ResolvedType) -> bool {
        matches!(
            r#type,
            ResolvedType::Bool
                | ResolvedType::Char
                | ResolvedType::I8
                | ResolvedType::I16
                | ResolvedType::I32
                | ResolvedType::I64
                | ResolvedType::ISize
                | ResolvedType::U8
                | ResolvedType::U16
                | ResolvedType::U32
                | ResolvedType::U64
                | ResolvedType::USize
                | ResolvedType::F32
                | ResolvedType::F64
                | ResolvedType::Pointer { .. }
        )
    }

    fn infer_reg_binding_name(expr: &HirExprNode) -> Option<Ident> {
        match &expr.expr {
            HirExpr::Place(HirPlace {
                root: HirPlaceRoot::Path(path),
                projections,
            }) if projections.is_empty()
                && path.path.tail.is_empty()
                && path.generic_args.is_empty()
                && path.qualified_spec.is_none() =>
            {
                Some(path.path.head.clone())
            }
            HirExpr::AddressOf(addr) => Self::infer_reg_binding_name(&addr.base),
            _ => None,
        }
    }

    fn render_asm_const(value: &ConstValue) -> Option<String> {
        match value {
            ConstValue::Number(NumberValue::Signed(n)) => Some(n.to_string()),
            ConstValue::Number(NumberValue::Unsigned(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    /// Bindable descriptors are `reg`/`const` in source order; `clobber`
    /// never participates in `$name`/`$N` binding.
    fn validate_asm_bindings(
        &mut self,
        asm_id: HirId,
        body: &str,
        body_span: Span,
        descriptors: &[CheckedAsmDescriptor],
    ) -> bool {
        let bindable: Vec<&CheckedAsmDescriptor> = descriptors
            .iter()
            .filter(|d| !matches!(d.kind, CheckedAsmDescriptorKind::Clobber { .. }))
            .collect();

        let mut name_index: HashMap<&str, Option<usize>> = HashMap::new();
        for (i, d) in bindable.iter().enumerate() {
            if let Some(name) = &d.binding_name {
                name_index
                    .entry(name.as_ref())
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(i));
            }
        }

        let mut ok = true;
        for reference in scan_asm_bindings(body, body_span.start) {
            match reference {
                AsmBindingRef::Named { text, span } => {
                    match name_index.get(text.trim_start_matches('$')) {
                        Some(Some(_)) => {}
                        Some(None) => {
                            self.error(asm_id, span, AnalysisErrorKind::AsmAmbiguousBinding { text });
                            ok = false;
                        }
                        None => {
                            self.error(asm_id, span, AnalysisErrorKind::AsmUnknownBinding { text });
                            ok = false;
                        }
                    }
                }
                AsmBindingRef::Positional { index, span } => {
                    if (index as usize) >= bindable.len() {
                        self.error(
                            asm_id,
                            span,
                            AnalysisErrorKind::AsmUnknownBinding {
                                text: format!("${index}"),
                            },
                        );
                        ok = false;
                    }
                }
            }
        }
        ok
    }
}
