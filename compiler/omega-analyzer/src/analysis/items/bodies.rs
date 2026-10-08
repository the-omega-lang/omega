use super::*;
use crate::compiler_functions::CompilerFunction;

impl<'r> Analyzer<'r> {
    pub fn check_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedFunctionDef> {
        let f = &self.normalized_function(f)?;
        let Some(body) = &f.body else {
            return self.check_compiler_function_body(f, fn_type, id, annotations);
        };
        if annotations.naked {
            return self.check_naked_function_body(f, body, fn_type, id, annotations);
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
                let body = this.analyze_block(body, Expected::Exact(fn_type.return_type.as_ref()));
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
            generic_args: vec![],
            self_mode: f.self_mode,
            is_variadic: fn_type.is_variadic,
            params,
            return_type: (*fn_type.return_type).clone(),
            body,
            inline: annotations.inline,
            symbol: annotations.symbol.clone(),
            conformance_owner: None,
            primitive_target: None,
            method_owner: None,
            template: None,
            naked: annotations.naked,
            runtime_checks: None,
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
        body: &HirBlock,
        fn_type: &ResolvedFunctionType,
        id: HirId,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedFunctionDef> {
        let (params, asm) = self.with_suppressed(&annotations.suppress, |this| {
            let ((params, asm), _scope) = this.with_scope(|this| {
                let params = this.analyze_all(&f.params, Self::analyze_param);
                this.current_return_type = (*fn_type.return_type).clone();
                let asm = this.analyze_naked_body(f, body);
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
            generic_args: vec![],
            self_mode: f.self_mode,
            is_variadic: fn_type.is_variadic,
            params,
            return_type: (*fn_type.return_type).clone(),
            body: CheckedBlock {
                stmts: vec![CheckedStmt::InlineAsm(asm)],
                tail: None,
            },
            inline: annotations.inline,
            symbol: annotations.symbol.clone(),
            conformance_owner: None,
            primitive_target: None,
            method_owner: None,
            template: None,
            naked: true,
            runtime_checks: None,
        })
    }

    fn analyze_naked_body(
        &mut self,
        f: &HirFunctionDef,
        body: &HirBlock,
    ) -> Option<CheckedInlineAsm> {
        let [HirStmt::InlineAsm(asm)] = body.stmts.as_slice() else {
            self.error(f.id, f.span, AnalysisErrorKind::InvalidNakedBody);
            return None;
        };
        if body.tail.is_some() {
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

    /// A bodyless declaration reaching here was already classified by the
    /// driver's signature sweep; its body is built over the concrete
    /// instance's types.
    fn check_compiler_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedFunctionDef> {
        let function = match crate::compiler_functions::compiler_function(&self.module_path, f) {
            Ok(Some(function)) => function,
            Ok(None) => unreachable!("a bodyless function always classifies or errors"),
            Err(kind) => {
                self.error(f.id, f.span, kind);
                return None;
            }
        };
        let (params, _scope) =
            self.with_scope(|this| this.analyze_all(&f.params, Self::analyze_param));
        let params = params?;

        let kind = match function {
            CompilerFunction::ReadVolatile | CompilerFunction::WriteVolatile => {
                Self::volatile_access(function, &params)
            }
            CompilerFunction::TypeInfo => {
                let described = Type::Named(Path::from(f.generics[0].ident.clone()));
                let described = self.resolve_type_or_error(f.id, f.span, &described, false)?;
                CheckedExpr::Const(crate::reflection::type_info_ref(&described))
            }
        };
        let tail = CheckedExprNode {
            id: f.id,
            span: f.span,
            r#type: (*fn_type.return_type).clone(),
            kind,
        };

        Some(CheckedFunctionDef {
            id,
            span: f.span,
            name: f.name.clone(),
            generic_args: vec![],
            self_mode: f.self_mode,
            is_variadic: fn_type.is_variadic,
            params,
            return_type: (*fn_type.return_type).clone(),
            body: CheckedBlock {
                stmts: Vec::new(),
                tail: Some(Box::new(tail)),
            },
            inline: annotations.inline,
            symbol: annotations.symbol.clone(),
            conformance_owner: None,
            primitive_target: None,
            method_owner: None,
            template: None,
            naked: false,
            runtime_checks: None,
        })
    }

    /// One volatile access through the `location` parameter.
    fn volatile_access(function: CompilerFunction, params: &[CheckedParam]) -> CheckedExpr {
        let parameter = |param: &CheckedParam| CheckedPlace {
            root: CheckedPlaceRoot::Variable {
                decl_id: param.id,
                storage: Storage::Parameter,
                r#type: param.r#type.clone(),
            },
            projections: Vec::new(),
            r#type: param.r#type.clone(),
        };
        let location = &params[0];
        let ResolvedType::Pointer { pointee, .. } = &location.r#type else {
            unreachable!("the classifier only accepts a pointer 'location' parameter");
        };
        let target = CheckedPlace {
            projections: vec![CheckedProjection::Deref {
                r#type: (**pointee).clone(),
            }],
            r#type: (**pointee).clone(),
            ..parameter(location)
        };
        match function {
            CompilerFunction::ReadVolatile => CheckedExpr::VolatileRead(target),
            CompilerFunction::WriteVolatile => {
                let value = &params[1];
                CheckedExpr::VolatileWrite(CheckedAssignment {
                    target,
                    value: Box::new(CheckedExprNode {
                        id: value.id,
                        span: value.span,
                        r#type: value.r#type.clone(),
                        kind: CheckedExpr::Place(parameter(value)),
                    }),
                })
            }
            CompilerFunction::TypeInfo => unreachable!("not a volatile access"),
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
            body: Some(body),
        };
        self.check_function_body(
            &synthetic,
            &pending.fn_type,
            pending.id,
            &crate::annotations::ResolvedAnnotations::default(),
        )
    }
    fn check_method_bodies(
        &mut self,
        functions: &[omega_hir::HirFunctionDef],
        methods: &[(Ident, ResolvedMethod)],
        suppress: &[Ident],
    ) -> Option<Vec<CheckedFunctionDef>> {
        // Only the declarations `collect_methods` resolved concretely have a
        // signature here, and the two lists agree on declaration order, so a
        // generic template is skipped on both sides rather than shifting the
        // pairing of every method after it.
        let functions = self.concrete_methods(functions);
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
        let (fields, methods, suppress) = {
            let resolved = cell.borrow();
            (
                Self::checked_fields(&s.fields, &resolved.fields),
                resolved.functions.clone(),
                resolved.suppress.clone(),
            )
        };
        let functions = self.check_method_bodies(&s.functions, &methods, &suppress)?;
        Some(CheckedStructDef {
            id: s.id,
            span: s.span,
            name: s.name.clone(),
            generic_args: vec![],
            fields,
            functions,
        })
    }

    pub fn check_union_body(
        &mut self,
        u: &HirUnionDef,
        cell: &Rc<RefCell<ResolvedUnionType>>,
    ) -> Option<CheckedUnionDef> {
        let (fields, methods, suppress) = {
            let resolved = cell.borrow();
            (
                Self::checked_fields(&u.fields, &resolved.fields),
                resolved.functions.clone(),
                resolved.suppress.clone(),
            )
        };
        let functions = self.check_method_bodies(&u.functions, &methods, &suppress)?;
        Some(CheckedUnionDef {
            id: u.id,
            span: u.span,
            name: u.name.clone(),
            generic_args: vec![],
            fields,
            functions,
        })
    }

    pub fn check_enum_body(
        &mut self,
        e: &HirEnumDef,
        cell: &Rc<RefCell<ResolvedEnumType>>,
    ) -> Option<CheckedEnumDef> {
        let (methods, suppress) = {
            let resolved = cell.borrow();
            (resolved.functions.clone(), resolved.suppress.clone())
        };
        let functions = self.check_method_bodies(&e.functions, &methods, &suppress)?;
        Some(CheckedEnumDef {
            id: e.id,
            span: e.span,
            name: e.name.clone(),
            generic_args: vec![],
            functions,
        })
    }
}
