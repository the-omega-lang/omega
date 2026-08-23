use super::*;

impl<'r> Analyzer<'r> {
    pub fn check_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedFunctionDef> {
        let f = &self.normalized_function(f)?;
        if annotations.naked {
            return self.check_naked_function_body(f, fn_type, id, annotations);
        }

        let (params, body) = self.with_suppressed(&annotations.suppress, |this| {
            if annotations.inline.is_some() {
                this.warn(f.id, f.span, AnalysisWarningKind::InlineNotEnforced);
            }

            let ((params, body), scope) = this.with_scope(|this| {
                let params = this.analyze_all(&f.params, Self::analyze_param);
                this.current_return_type = (*fn_type.return_type).clone();
                debug_assert!(
                    this.loop_stack.is_empty(),
                    "loop state must not leak between function bodies"
                );
                debug_assert!(
                    !this.in_defer_body,
                    "defer state must not leak between function bodies"
                );
                let body = this.analyze_block(&f.body, Some(fn_type.return_type.as_ref()));
                (params, body)
            });
            this.warn_unused_bindings(scope, true);
            (params, body)
        });

        let params = params?;
        let body = body?;
        self.check_function_return(f.id, f.return_type_span, &fn_type.return_type, &body)?;

        Some(CheckedFunctionDef {
            id,
            span: f.span,
            name: f.name.clone(),
            type_args: vec![],
            self_mode: f.self_mode,
            is_variadic: fn_type.is_variadic,
            params,
            return_type: (*fn_type.return_type).clone(),
            body,
            inline: annotations.inline,
            mangling: annotations.mangling.clone(),
            conformance_owner: None,
            primitive_target: None,
            naked: annotations.naked,
        })
    }

    /// A `@naked` body skips ordinary block/return analysis: parameters are
    /// ABI-only (no locals, no unused-parameter warnings), and the body must
    /// be exactly the user's single `asm` statement, which is validated with
    /// a policy that forbids `reg(...)` since it would force LLVM to
    /// materialize a runtime register operand around a body that is supposed
    /// to own the entire function.
    fn check_naked_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedFunctionDef> {
        let (params, asm) = self.with_suppressed(&annotations.suppress, |this| {
            let ((params, asm), _scope) = this.with_scope(|this| {
                let params = this.analyze_all(&f.params, Self::analyze_param);
                this.current_return_type = (*fn_type.return_type).clone();
                let asm = this.analyze_naked_body(f);
                (params, asm)
            });
            (params, asm)
        });

        let params = params?;
        let asm = asm?;

        Some(CheckedFunctionDef {
            id,
            span: f.span,
            name: f.name.clone(),
            type_args: vec![],
            self_mode: f.self_mode,
            is_variadic: fn_type.is_variadic,
            params,
            return_type: (*fn_type.return_type).clone(),
            body: CheckedBlock {
                stmts: vec![CheckedStmt::InlineAsm(asm)],
                tail: None,
            },
            inline: annotations.inline,
            mangling: annotations.mangling.clone(),
            conformance_owner: None,
            primitive_target: None,
            naked: true,
        })
    }

    fn analyze_naked_body(&mut self, f: &HirFunctionDef) -> Option<CheckedInlineAsm> {
        let [HirStmt::InlineAsm(asm)] = f.body.stmts.as_slice() else {
            self.error(f.id, f.span, AnalysisErrorKind::InvalidNakedBody);
            return None;
        };
        if f.body.tail.is_some() {
            self.error(f.id, f.span, AnalysisErrorKind::InvalidNakedBody);
            return None;
        }

        let previous = std::mem::replace(&mut self.in_naked_asm, true);
        let stmts = self.analyze_inline_asm(asm);
        self.in_naked_asm = previous;

        match stmts?.into_iter().next() {
            Some(CheckedStmt::InlineAsm(checked)) => Some(checked),
            _ => unreachable!("analyze_inline_asm always yields exactly one InlineAsm statement"),
        }
    }

    pub fn check_pending_spec_method(
        &mut self,
        pending: &PendingSpecMethod,
    ) -> Option<CheckedFunctionDef> {
        let body = pending
            .raw
            .default_body
            .clone()
            .expect("only ever queued by conformance when a default body exists");
        let synthetic = HirFunctionDef {
            id: pending.raw.decl_id,
            span: pending.raw.span,
            name_span: pending.raw.name_span,
            signature_span: pending.raw.signature_span,
            return_type_span: pending.raw.return_type_span,
            annotations: Vec::new(),
            visibility: pending.raw.visibility,
            explicit_hidden_span: None,
            name: pending.raw.name.clone(),
            generics: vec![],
            self_mode: pending.raw.self_mode,
            params: pending.raw.params.clone(),
            return_type: pending.raw.return_type.clone(),
            body,
        };
        // Mirrors `with_owner` in `check_struct_body`/`check_union_body`/
        // `check_enum_body`: a default body calling a hidden sibling
        // requirement (`self.other()`) is checked against the conforming
        // type's owner, not the spec's -- `require_method_visible` resolves
        // a method's owner from the receiver type (`Self`, substituted to
        // the conforming type here), never from the declaring spec.
        let owner = pending
            .substitution
            .first()
            .and_then(|(_, self_type)| self_type.declaring_owner())
            .map(|(_, owner_id)| owner_id);
        let check = |this: &mut Self| {
            this.check_function_body(
                &synthetic,
                &pending.fn_type,
                pending.id,
                &crate::annotations::ResolvedAnnotations::default(),
            )
        };
        match owner {
            Some(owner) => self.with_owner(owner, check),
            None => check(self),
        }
    }
    fn check_method_bodies(
        &mut self,
        functions: &[omega_hir::HirFunctionDef],
        methods: &[(Ident, ResolvedMethod)],
        suppress: &[Ident],
    ) -> Option<Vec<CheckedFunctionDef>> {
        self.with_suppressed(suppress, |this| {
            let mut checked = Vec::with_capacity(functions.len());
            let mut ok = true;
            for (function, (_, method)) in functions.iter().zip(methods) {
                match this.check_function_body(
                    function,
                    &method.fn_type,
                    method.decl_id,
                    &method.annotations,
                ) {
                    Some(body) => checked.push(body),
                    None => ok = false,
                }
            }
            ok.then_some(checked)
        })
    }

    fn checked_fields(declared: &[HirField], resolved: &[ResolvedField]) -> Vec<CheckedField> {
        declared
            .iter()
            .zip(resolved)
            .map(|(field, resolved)| CheckedField {
                id: field.id,
                span: field.span,
                ident: field.ident.clone(),
                r#type: resolved.r#type.clone(),
            })
            .collect()
    }

    pub fn check_struct_body(
        &mut self,
        s: &HirStructDef,
        cell: &Rc<RefCell<ResolvedStructType>>,
    ) -> Option<CheckedStructDef> {
        let (owner, fields, methods, suppress) = {
            let resolved = cell.borrow();
            (
                resolved.id,
                Self::checked_fields(&s.fields, &resolved.fields),
                resolved.functions.clone(),
                resolved.suppress.clone(),
            )
        };
        let functions = self.with_owner(owner, |this| {
            this.check_method_bodies(&s.functions, &methods, &suppress)
        })?;
        Some(CheckedStructDef {
            id: s.id,
            span: s.span,
            name: s.name.clone(),
            type_args: vec![],
            fields,
            functions,
        })
    }

    pub fn check_union_body(
        &mut self,
        u: &HirUnionDef,
        cell: &Rc<RefCell<ResolvedUnionType>>,
    ) -> Option<CheckedUnionDef> {
        let (owner, fields, methods, suppress) = {
            let resolved = cell.borrow();
            (
                resolved.id,
                Self::checked_fields(&u.fields, &resolved.fields),
                resolved.functions.clone(),
                resolved.suppress.clone(),
            )
        };
        let functions = self.with_owner(owner, |this| {
            this.check_method_bodies(&u.functions, &methods, &suppress)
        })?;
        Some(CheckedUnionDef {
            id: u.id,
            span: u.span,
            name: u.name.clone(),
            type_args: vec![],
            fields,
            functions,
        })
    }

    pub fn check_enum_body(
        &mut self,
        e: &HirEnumDef,
        cell: &Rc<RefCell<ResolvedEnumType>>,
    ) -> Option<CheckedEnumDef> {
        let (owner, methods, suppress) = {
            let resolved = cell.borrow();
            (
                resolved.id,
                resolved.functions.clone(),
                resolved.suppress.clone(),
            )
        };
        let functions = self.with_owner(owner, |this| {
            this.check_method_bodies(&e.functions, &methods, &suppress)
        })?;
        Some(CheckedEnumDef {
            id: e.id,
            span: e.span,
            name: e.name.clone(),
            type_args: vec![],
            functions,
        })
    }
}
