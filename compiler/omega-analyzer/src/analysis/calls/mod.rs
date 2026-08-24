use super::*;

mod generic;
mod overload;
mod spec;

pub(crate) enum Intercepted {
    Declined,
    Claimed(Option<CheckedExprNode>),
}

pub(super) type Interceptor<'r> =
    fn(&mut Analyzer<'r>, HirId, Span, &HirFunctionCall, Option<&ResolvedType>) -> Intercepted;

#[derive(Clone)]
struct Receiver {
    place: HirPlace,
    checked: CheckedPlace,
    r#type: ResolvedType,
    mutable: bool,
}

pub(super) struct ResolvedCallee {
    pub(super) callee: CheckedExprNode,
    pub(super) fn_type: ResolvedFunctionType,
    pub(super) implicit_self: Option<CheckedExprNode>,
    pub(super) checked_args: Option<Vec<CheckedExprNode>>,
}

pub(super) enum CalleeResolution {
    Ordinary(ResolvedCallee),
    Dynamic(Option<CheckedExprNode>),
}

impl<'r> Analyzer<'r> {
    fn analyze_receiver_operand(&mut self, operand: &HirExprNode) -> Option<Receiver> {
        self.with_reveal_operand(operand, |this, inner| {
            let place = match &inner.expr {
                HirExpr::Place(place) => place.clone(),
                _ => HirPlace {
                    root: HirPlaceRoot::Expr(Box::new(inner.clone())),
                    projections: vec![],
                },
            };
            let (checked, r#type, mutable) =
                this.analyze_place(inner.id, inner.span, &place, None)?;
            if let CheckedPlaceRoot::Variable { decl_id, .. } = checked.root {
                this.context.mark_used(decl_id);
            }
            Some(Receiver {
                place,
                checked,
                r#type,
                mutable,
            })
        })
    }

    fn split_item_path(absolute: &[Ident]) -> Option<(Ident, Vec<Ident>)> {
        absolute
            .split_last()
            .map(|(name, module)| (name.clone(), module.to_vec()))
    }

    fn callee_path(call: &HirFunctionCall) -> Option<&Path> {
        let HirExpr::Place(place) = &Self::strip_reveal(&call.callee).1.expr else {
            return None;
        };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else {
            return None;
        };
        expr_path.plain()
    }

    fn checked_call(
        &self,
        node_id: HirId,
        span: Span,
        callee_site: &HirExprNode,
        decl_id: HirId,
        storage: Storage,
        fn_type: ResolvedFunctionType,
        args: Vec<CheckedExprNode>,
    ) -> CheckedExprNode {
        let function = ResolvedType::Function(fn_type.clone());
        let callee = CheckedExprNode {
            id: callee_site.id,
            span: callee_site.span,
            r#type: function.clone(),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable {
                    decl_id,
                    storage,
                    r#type: function.clone(),
                },
                projections: vec![],
                r#type: function,
            }),
        };
        CheckedExprNode {
            id: node_id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall {
                callee: Box::new(callee),
                fn_type,
                args,
            }),
        }
    }

    pub(super) fn resolve_callee(
        &mut self,
        callee: &HirExprNode,
        args: &[HirExprNode],
    ) -> Option<CalleeResolution> {
        // The call analyzer owns the reveal bypass for the whole callee
        // resolution. This function only needs the transparent inner shape.
        let (_, callee) = Self::strip_reveal(callee);

        let member = match &callee.expr {
            HirExpr::Place(place) => match place.projections.last() {
                Some(HirProjection::FieldAccess(field)) => Some((place, field)),
                _ => None,
            },
            _ => None,
        };
        let Some((place, field)) = member else {
            let checked = self.analyze_expr(callee, None)?;
            let fn_type = self.require_callable(callee.id, callee.span, checked.r#type.clone())?;
            return Some(CalleeResolution::Ordinary(ResolvedCallee {
                callee: checked,
                fn_type,
                implicit_self: None,
                checked_args: None,
            }));
        };

        let base_place = HirPlace {
            root: place.root.clone(),
            projections: place.projections[..place.projections.len() - 1].to_vec(),
        };
        let (mut checked, mut r#type, mut mutable) =
            self.analyze_place(callee.id, callee.span, &base_place, None)?;
        // A method call names something on the member, so a refined
        // anonymous receiver opens onto its payload before method lookup.
        if let Some(member) =
            Self::open_refined_anonymous(&mut checked.projections, &r#type, &mut mutable)
        {
            checked.r#type = member.clone();
            r#type = member;
        }
        // A method call reads its receiver, so `x` in `x.method()` is used.
        // Marked here rather than inside `analyze_place` (which also serves
        // assignment targets, where marking used would wrongly silence
        // `UnusedVariable` on write-only bindings).
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked.root {
            self.context.mark_used(decl_id);
        }
        let receiver = Receiver {
            place: base_place,
            checked,
            r#type,
            mutable,
        };

        if let ResolvedType::SpecObject { shape, .. } = &receiver.r#type {
            let shape = shape.clone();
            return Some(CalleeResolution::Dynamic(
                self.finish_dynamic_dispatch_call(
                    callee.id,
                    callee.span,
                    receiver.checked,
                    &shape,
                    field,
                    args,
                ),
            ));
        }

        let members = self.find_functions(
            callee.id,
            callee.span,
            &receiver.r#type,
            field,
            FunctionNamespace::Member,
        );
        if members.is_empty() {
            let receiver_type = receiver.r#type.autoderef();
            let field_shadows = match receiver_type {
                ResolvedType::Struct(cell) => cell
                    .borrow()
                    .fields
                    .iter()
                    .any(|candidate| &candidate.name == field),
                ResolvedType::Union(cell) => cell
                    .borrow()
                    .fields
                    .iter()
                    .any(|candidate| &candidate.name == field),
                ResolvedType::Enum { cell, variant } => {
                    let e = cell.borrow();
                    field.as_ref() == "tag"
                        || e.header.iter().any(|candidate| &candidate.name == field)
                        || variant.is_some_and(|i| {
                            e.variants[i]
                                .fields
                                .iter()
                                .any(|candidate| &candidate.name == field)
                        })
                }
                _ => false,
            };
            if !field_shadows {
                // The type's own receiverless declaration is the precise
                // diagnosis for `value.name(...)`; it is probed only here,
                // never merged into the member candidate set.
                if !self
                    .find_functions(
                        callee.id,
                        callee.span,
                        &receiver.r#type,
                        field,
                        FunctionNamespace::Static,
                    )
                    .is_empty()
                {
                    self.error(
                        callee.id,
                        callee.span,
                        AnalysisErrorKind::StaticFunctionOnInstance {
                            r#struct: Self::owner_name(&receiver.r#type),
                            function: field.clone(),
                        },
                    );
                    return None;
                }
                match self.resolver.conformances_for_type(receiver_type) {
                    Ok(conformances) => {
                        if let Some(conform) = conformances.iter().find(|conform| {
                            !FunctionNamespace::Member
                                .select(&conform.methods, field)
                                .is_empty()
                        }) {
                            self.error(
                                callee.id,
                                callee.span,
                                AnalysisErrorKind::MethodNotInScope {
                                    method: field.clone(),
                                    spec: conform.spec.borrow().name.clone(),
                                    r#type: receiver_type.clone(),
                                },
                            );
                            return None;
                        }
                    }
                    Err(err) => {
                        self.error(
                            callee.id,
                            callee.span,
                            AnalysisErrorKind::ModuleResolution(err),
                        );
                        return None;
                    }
                }
            }
            return self.resolve_field_callee(callee, field, receiver);
        }
        self.resolve_method_callee(callee, field, receiver, members, args)
    }

    fn resolve_method_callee(
        &mut self,
        callee: &HirExprNode,
        field: &Ident,
        receiver: Receiver,
        methods: Vec<ResolvedMethod>,
        args: &[HirExprNode],
    ) -> Option<CalleeResolution> {
        let (method, checked_args) = self.pick_method(callee, field, methods, args)?;
        self.require_method_visible(callee, field, &receiver.r#type, &method)?;

        let self_mode = method
            .fn_type
            .self_mode
            .expect("a member candidate always has a self mode");
        let self_arg = self.adapt_self_argument(callee, receiver, self_mode)?;

        let fn_type = ResolvedType::Function(method.fn_type.clone());
        let callee_expr = CheckedExprNode {
            id: callee.id,
            span: callee.span,
            r#type: fn_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable {
                    decl_id: method.decl_id,
                    storage: Storage::Function,
                    r#type: fn_type.clone(),
                },
                projections: vec![],
                r#type: fn_type,
            }),
        };
        Some(CalleeResolution::Ordinary(ResolvedCallee {
            callee: callee_expr,
            fn_type: method.fn_type,
            implicit_self: Some(self_arg),
            checked_args,
        }))
    }

    /// Instance syntax supplies its receiver implicitly, so overloads are
    /// separated by the arguments actually written -- the receiver
    /// parameter is dropped from every candidate before ranking.
    fn pick_method(
        &mut self,
        callee: &HirExprNode,
        field: &Ident,
        members: Vec<ResolvedMethod>,
        args: &[HirExprNode],
    ) -> Option<(ResolvedMethod, Option<Vec<CheckedExprNode>>)> {
        if let [only] = members.as_slice() {
            return Some((only.clone(), None));
        }
        let candidates: Vec<(HirId, ResolvedFunctionType)> = members
            .iter()
            .map(|m| {
                let mut sans_self = m.fn_type.clone();
                sans_self.params = sans_self.params[1..].to_vec();
                (m.decl_id, sans_self)
            })
            .collect();
        let (winner, checked) =
            self.resolve_overload(callee.id, callee.span, field, &candidates, args)?;
        Some((members[winner].clone(), Some(checked)))
    }

    fn owner_name(receiver_type: &ResolvedType) -> Ident {
        match receiver_type.autoderef() {
            ResolvedType::Struct(cell) => cell.borrow().name.clone(),
            ResolvedType::Union(cell) => cell.borrow().name.clone(),
            ResolvedType::Enum { cell, .. } => cell.borrow().name.clone(),
            other => Ident(other.to_string()),
        }
    }

    fn require_method_visible(
        &mut self,
        callee: &HirExprNode,
        field: &Ident,
        receiver_type: &ResolvedType,
        method: &ResolvedMethod,
    ) -> Option<()> {
        let (module_path, owner_id) = receiver_type
            .autoderef()
            .declaring_owner()
            .unwrap_or_else(|| (Vec::new(), callee.id));
        let visible = self.check_member_visibility(method.visibility, &module_path, owner_id);
        if visible {
            return Some(());
        }
        self.error(
            callee.id,
            callee.span,
            AnalysisErrorKind::MethodNotVisible {
                method: field.clone(),
                base: receiver_type.clone(),
            },
        );
        None
    }

    fn adapt_self_argument(
        &mut self,
        callee: &HirExprNode,
        receiver: Receiver,
        self_mode: SelfMode,
    ) -> Option<CheckedExprNode> {
        let (id, span) = (callee.id, callee.span);
        let generated_id = self.resolver.fresh_synthetic_id();
        let wants_mutable = self_mode.is_mutable();
        let Receiver {
            place,
            checked,
            r#type,
            mutable,
        } = receiver;

        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = checked.root
        {
            return self.adapt_comp_self_argument(id, span, checked, r#type, self_mode);
        }

        let node = |r#type, kind| CheckedExprNode {
            id: generated_id,
            span,
            r#type,
            kind,
        };
        match (&r#type, self_mode.is_pointer()) {
            (
                ResolvedType::Pointer {
                    pointee,
                    mutable: pointer_mutable,
                },
                true,
            ) => {
                self.require_mutable_pointer(id, span, wants_mutable, *pointer_mutable)?;
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(pointee.widened()),
                    mutable: wants_mutable,
                };
                Some(node(pointer, CheckedExpr::Place(checked)))
            }
            (
                ResolvedType::Str {
                    mutable: base_mutable,
                },
                true,
            ) => {
                self.require_mutable_pointer(id, span, wants_mutable, *base_mutable)?;
                Some(node(
                    ResolvedType::Str {
                        mutable: wants_mutable,
                    },
                    CheckedExpr::Place(checked),
                ))
            }
            (
                ResolvedType::Slice {
                    item,
                    mutable: base_mutable,
                },
                true,
            ) => {
                self.require_mutable_pointer(id, span, wants_mutable, *base_mutable)?;
                let slice = ResolvedType::Slice {
                    item: item.clone(),
                    mutable: wants_mutable,
                };
                Some(node(slice, CheckedExpr::Place(checked)))
            }
            (_, true) => {
                if wants_mutable {
                    self.require_mutable_place(id, span, &place.root, &checked, mutable)?;
                    // De-assumption, like an explicit `&mut`.
                    if let Some((ident, origin, ..)) = self.narrowable_place(&place) {
                        self.context.widen_variable(&ident, origin);
                    }
                }
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(r#type.widened()),
                    mutable: wants_mutable,
                };
                Some(node(
                    pointer,
                    CheckedExpr::AddressOf(CheckedAddressOf { place: checked }),
                ))
            }
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let pointee = pointee.widened();
                let mut place = checked;
                place.projections.push(CheckedProjection::Deref {
                    r#type: pointee.clone(),
                });
                Some(node(pointee, CheckedExpr::Place(place)))
            }
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Place(checked))),
        }
    }

    fn adapt_comp_self_argument(
        &mut self,
        id: HirId,
        span: Span,
        checked: CheckedPlace,
        r#type: ResolvedType,
        self_mode: SelfMode,
    ) -> Option<CheckedExprNode> {
        if self_mode.is_pointer() && self_mode.is_mutable() {
            self.error(id, span, AnalysisErrorKind::NotMutablePointer);
            return None;
        }
        let value = self.resolve_comp_place(id, span, &checked)?;
        let generated_id = self.resolver.fresh_synthetic_id();
        let node = |r#type, kind| CheckedExprNode {
            id: generated_id,
            span,
            r#type,
            kind,
        };
        match (&r#type, self_mode.is_pointer()) {
            (ResolvedType::Pointer { pointee, .. }, true) => {
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(pointee.widened()),
                    mutable: false,
                };
                Some(node(pointer, CheckedExpr::Const(value)))
            }
            (ResolvedType::Str { .. }, true) => Some(node(
                ResolvedType::Str { mutable: false },
                CheckedExpr::Const(value),
            )),
            (ResolvedType::Slice { item, .. }, true) => {
                let slice = ResolvedType::Slice {
                    item: item.clone(),
                    mutable: false,
                };
                Some(node(slice, CheckedExpr::Const(value)))
            }
            (_, true) => {
                let pointer = ResolvedType::Pointer {
                    pointee: Box::new(r#type.widened()),
                    mutable: false,
                };
                Some(node(
                    pointer,
                    CheckedExpr::Const(ConstValue::Ref(Box::new(value))),
                ))
            }
            (ResolvedType::Pointer { pointee, .. }, false) => {
                let ConstValue::Ref(inner) = value else {
                    unreachable!(
                        "a comp value's own type is only ever Pointer alongside a ConstValue::Ref -- see ConstValue::Ref's doc comment"
                    );
                };
                Some(node(pointee.widened(), CheckedExpr::Const(*inner)))
            }
            (_, false) => Some(node(r#type.widened(), CheckedExpr::Const(value))),
        }
    }

    fn require_mutable_pointer(
        &mut self,
        id: HirId,
        span: Span,
        wants_mutable: bool,
        is_mutable: bool,
    ) -> Option<()> {
        if wants_mutable && !is_mutable {
            self.error(id, span, AnalysisErrorKind::NotMutablePointer);
            return None;
        }
        Some(())
    }

    fn resolve_field_callee(
        &mut self,
        callee: &HirExprNode,
        field: &Ident,
        receiver: Receiver,
    ) -> Option<CalleeResolution> {
        if let CheckedPlaceRoot::Variable {
            storage: Storage::Comp,
            ..
        } = receiver.checked.root
        {
            self.error(
                callee.id,
                callee.span,
                AnalysisErrorKind::CompEvalFailed {
                    reason:
                        "calling a function stored in a 'comp' binding's field isn't supported yet"
                            .into(),
                    trace: vec![],
                },
            );
            return None;
        }
        let CheckedPlace {
            root,
            mut projections,
            r#type: _,
        } = receiver.checked;
        let base_type = receiver.r#type;
        let field_type = self.resolve_field_projection(
            callee.id,
            callee.span,
            &mut projections,
            &base_type,
            field,
            &mut false,
        )?;
        let checked = CheckedExprNode {
            id: callee.id,
            span: callee.span,
            r#type: field_type.clone(),
            kind: CheckedExpr::Place(CheckedPlace {
                root,
                projections,
                r#type: field_type.clone(),
            }),
        };
        let fn_type = self.require_callable(callee.id, callee.span, field_type)?;
        Some(CalleeResolution::Ordinary(ResolvedCallee {
            callee: checked,
            fn_type,
            implicit_self: None,
            checked_args: None,
        }))
    }

    fn require_callable(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
    ) -> Option<ResolvedFunctionType> {
        match r#type {
            // Invoking the value is what relies on the pointed-to signature's
            // ABI; merely storing or passing the function pointer does not.
            ResolvedType::Function(fn_type) => self
                .check_signature_abi(id, span, &fn_type)
                .then_some(fn_type),
            _ => {
                self.error(id, span, AnalysisErrorKind::UnresolvedCallee);
                None
            }
        }
    }

    fn finish_dynamic_dispatch_call(
        &mut self,
        id: HirId,
        span: Span,
        base: CheckedPlace,
        shape: &crate::resolved_type::ResolvedSpecShape,
        field: &Ident,
        args: &[HirExprNode],
    ) -> Option<CheckedExprNode> {
        let self_placeholder = ResolvedType::Void;
        // Concatenate every member's flattened functions in canonical shape
        // order -- this is exactly the vtable section layout, so the index
        // found here doubles as the global vtable slot.
        let mut flattened = Vec::new();
        for member in &shape.members {
            flattened.extend(self.flatten_spec(
                id,
                span,
                &member.spec,
                &member.spec_args,
                &self_placeholder,
            )?);
        }
        let matches: Vec<usize> = flattened
            .iter()
            .enumerate()
            .filter(|(_, f)| &f.name == field)
            .map(|(index, _)| index)
            .collect();
        let slot_index = match matches.as_slice() {
            [] => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::NoSuchSpecFunction {
                        spec: Ident(shape.to_string()),
                        function: field.clone(),
                    },
                );
                return None;
            }
            [index] => *index,
            // Two of this object's specs declare the same function name --
            // must not silently pick the first slot (static dispatch through
            // a conjunction bound already rejects the identical shape).
            _ => {
                let specs: Vec<Ident> = matches
                    .iter()
                    .map(|&index| flattened[index].spec_name.clone())
                    .collect();
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::AmbiguousSpecObjectMethod {
                        function: field.clone(),
                        specs,
                    },
                );
                return None;
            }
        };
        let fn_type = flattened[slot_index].fn_type.clone();
        let param_types = fn_type.params[1..].to_vec();

        if args.len() != param_types.len() {
            self.error(
                id,
                span,
                AnalysisErrorKind::WrongArgumentCount {
                    expected: param_types.len(),
                    found: args.len(),
                },
            );
            return None;
        }

        let mut checked_args = Vec::with_capacity(args.len());
        let mut ok = true;
        for (arg, (_, expected_type)) in args.iter().zip(&param_types) {
            let Some(checked_arg) = self.analyze_expr(arg, Some(expected_type)) else {
                ok = false;
                continue;
            };
            let checked_arg = self.coerce_to_expected(Some(expected_type), checked_arg);
            if !expected_type.accepts(&checked_arg.r#type) {
                self.error(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: checked_arg.r#type.clone(),
                    },
                );
                ok = false;
                continue;
            }
            checked_args.push(checked_arg);
        }
        if !ok {
            return None;
        }

        Some(CheckedExprNode {
            id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::DynamicCall(CheckedDynamicCall {
                base,
                slot_index,
                fn_type,
                args: checked_args,
            }),
        })
    }
}
