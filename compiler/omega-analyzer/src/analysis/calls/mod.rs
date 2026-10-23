use super::*;

mod generic;
mod inference;
mod overload;
mod pattern;
mod selector;
mod spec;
#[cfg(test)]
mod tests;
mod value;

use generic::MethodTemplate;
pub(crate) use inference::Inference;
pub(crate) use selector::{ValidatedGenerics, WrittenGenerics};
pub(crate) use value::FunctionValue;

#[derive(Clone, Copy)]
pub(crate) struct MemberGenerics<'p> {
    pub(crate) owner: Option<&'p [ExprGenericArg]>,
    pub(crate) function: Option<&'p [ExprGenericArg]>,
}

pub(crate) enum Intercepted {
    Declined,
    Claimed(Option<CheckedExprNode>),
}

pub(super) type Interceptor<'r> =
    fn(&mut Analyzer<'r>, HirId, Span, &HirFunctionCall, Expected<'_>) -> Intercepted;

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

/// Where a member call's callee, `receiver.member`, was written.
#[derive(Clone, Copy)]
pub(super) struct CallSite {
    pub(super) id: HirId,
    pub(super) span: Span,
}

pub(super) enum CalleeResolution {
    Ordinary(ResolvedCallee),
    Dynamic(Option<CheckedExprNode>),
}

impl<'r> Analyzer<'r> {
    fn analyze_receiver_operand(&mut self, operand: &HirExprNode) -> Option<Receiver> {
        let place = match &operand.expr {
            HirExpr::Place(place) => place.clone(),
            _ => HirPlace {
                root: HirPlaceRoot::Expr(Box::new(operand.clone())),
                projections: vec![],
                reveals: vec![],
            },
        };
        let (checked, r#type, mutable) =
            self.analyze_place(operand.id, operand.span, &place, None)?;
        if let CheckedPlaceRoot::Variable { decl_id, .. } = checked.root {
            self.context.mark_used(decl_id);
        }
        Some(Receiver {
            place,
            checked,
            r#type,
            mutable,
        })
    }

    /// The arguments a path writes on its last segment -- the function or
    /// item it names. `None` when a list is written on any earlier segment.
    fn trailing_generic_args(expr_path: &ExprPath) -> Option<&[ExprGenericArg]> {
        let last = expr_path.path.tail.len();
        expr_path
            .generic_args
            .only_on(&[last])
            .then(|| expr_path.generic_args.args_on(last).unwrap_or_default())
    }

    /// The lists a type-qualified function path writes on its owner and on
    /// its function, the owner being the segment the `member_segments` follow.
    /// `None` when a list is written on any other segment.
    pub(crate) fn member_generics(
        expr_path: &ExprPath,
        member_segments: usize,
    ) -> Option<MemberGenerics<'_>> {
        let function_at = expr_path.path.tail.len();
        let owner_at = function_at.checked_sub(member_segments)?;
        let lists = &expr_path.generic_args;
        lists
            .only_on(&[owner_at, function_at])
            .then(|| MemberGenerics {
                owner: lists.args_on(owner_at),
                function: lists.args_on(function_at),
            })
    }

    fn callee_expr_path(call: &HirFunctionCall) -> Option<&ExprPath> {
        let HirExpr::Place(place) = &call.callee.expr else {
            return None;
        };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(expr_path) = &place.root else {
            return None;
        };
        Some(expr_path)
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

    /// The callee of an ordinary call: any expression whose value is a
    /// function, including a field holding one, as in `(value.f)(...)`.
    pub(super) fn resolve_callee(&mut self, callee: &HirExprNode) -> Option<ResolvedCallee> {
        let checked = self.analyze_expr(callee, Expected::None)?;
        let fn_type = self.require_callable(callee.id, callee.span, checked.r#type.clone())?;
        Some(ResolvedCallee {
            callee: checked,
            fn_type,
            implicit_self: None,
            checked_args: None,
        })
    }

    /// `receiver.member(...)`, which only ever selects a member function.
    pub(super) fn resolve_member_callee(
        &mut self,
        call: &HirMemberCall,
        expected: Expected<'_>,
    ) -> Option<CalleeResolution> {
        let site = CallSite {
            id: call.callee_id,
            span: call.callee_span,
        };
        let field = &call.member;
        let field_origin = call.member_origin;
        let generic_args = &call.generic_args;
        let args = &call.args;
        let (mut checked, mut r#type, mut mutable) =
            self.analyze_place(site.id, site.span, &call.receiver, None)?;
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
            place: call.receiver.clone(),
            checked,
            r#type,
            mutable,
        };

        if let ResolvedType::SpecObject { shape, .. } = &receiver.r#type {
            let shape = shape.clone();
            return Some(CalleeResolution::Dynamic(
                self.finish_dynamic_dispatch_call(
                    site.id,
                    site.span,
                    receiver.checked,
                    &shape,
                    field,
                    field_origin,
                    args,
                ),
            ));
        }

        let overloads = match self.resolver.method_overload_candidates(
            receiver.r#type.autoderef(),
            field,
            FunctionNamespace::Member,
        ) {
            Ok(candidates) => candidates,
            Err(error) => {
                self.error(
                    site.id,
                    site.span,
                    AnalysisErrorKind::ModuleResolution(error),
                );
                return None;
            }
        };
        if overloads.len() > 1
            && overloads
                .iter()
                .any(|candidate| candidate.template().is_some())
        {
            let (winner, instantiated, checked_args) = self.resolve_overload_candidates(
                site.id,
                site.span,
                field,
                &overloads,
                args,
                expected,
                generic_args,
                1,
            )?;
            let method = instantiated.or_else(|| {
                receiver
                    .r#type
                    .autoderef()
                    .candidates_in(self.resolver.types(), FunctionNamespace::Member, field)
                    .ok()??
                    .into_iter()
                    .find(|m| m.decl_id == overloads[winner].decl_id)
            })?;
            return self.finish_method_callee(
                site,
                field,
                field_origin,
                receiver,
                method,
                Some(checked_args),
            );
        }
        let members = self.find_functions(
            site.id,
            site.span,
            &receiver.r#type,
            field,
            FunctionNamespace::Member,
        );
        if members.is_empty() {
            // A generic method is not among its owner's resolved members --
            // it has no signature until this call's arguments give it one --
            // so it is looked up before the name is treated as missing.
            if let Some(resolution) = self.resolve_generic_method_callee(
                site,
                field,
                field_origin,
                &receiver,
                generic_args,
                args,
                expected,
            ) {
                return resolution;
            }
            self.report_missing_member_function(site, field, field_origin, &receiver);
            return None;
        }
        self.reject_member_generic_args(site, field, &receiver.r#type, generic_args)?;
        self.resolve_method_callee(site, field, field_origin, receiver, members, args)
    }

    /// Diagnoses `value.name(...)` that selects no member function, preferring
    /// the declaration the name does reach: a static function, a member of a
    /// conformance that is not in scope, or a field holding a function.
    fn report_missing_member_function(
        &mut self,
        site: CallSite,
        field: &Ident,
        field_origin: Origin,
        receiver: &Receiver,
    ) {
        let receiver_type = receiver.r#type.autoderef();
        // The type's own receiverless declaration is the precise diagnosis
        // for `value.name(...)`; it is probed only here, never merged into
        // the member candidate set.
        if !self
            .find_functions(
                site.id,
                site.span,
                &receiver.r#type,
                field,
                FunctionNamespace::Static,
            )
            .is_empty()
        {
            self.error(
                site.id,
                site.span,
                AnalysisErrorKind::StaticFunctionOnInstance {
                    r#struct: Self::owner_name(&receiver.r#type),
                    function: field.clone(),
                },
            );
            return;
        }
        match self.resolver.conformances_for_type(receiver_type) {
            Ok(conformances) => {
                if let Some(conform) = conformances.iter().find(|conform| {
                    !FunctionNamespace::Member
                        .select(&conform.methods, field)
                        .is_empty()
                }) {
                    self.error(
                        site.id,
                        site.span,
                        AnalysisErrorKind::MethodNotInScope {
                            method: field.clone(),
                            spec: conform.spec.name.clone(),
                            r#type: receiver_type.clone(),
                        },
                    );
                    return;
                }
            }
            Err(err) => {
                self.error(site.id, site.span, AnalysisErrorKind::ModuleResolution(err));
                return;
            }
        }
        let callable_field = self.without_diagnostics(|this| {
            let mut projections = receiver.checked.projections.clone();
            this.resolve_field_projection(
                site.id,
                site.span,
                &mut projections,
                &receiver.r#type,
                field,
                field_origin,
                &mut false,
            )
            .is_some_and(|r#type| matches!(r#type, ResolvedType::Function(_)))
        });
        self.error(
            site.id,
            site.span,
            AnalysisErrorKind::NoSuchMemberFunction {
                function: field.clone(),
                owner: receiver_type.widened(),
                callable_field,
                receiver: callable_field
                    .then(|| Self::written_place(&receiver.place))
                    .flatten(),
            },
        );
    }

    /// A place as written, when it is a plain path followed by field names.
    fn written_place(place: &HirPlace) -> Option<String> {
        let HirPlaceRoot::Path(path) = &place.root else {
            return None;
        };
        let path = path.plain().filter(|path| path.is_unqualified())?;
        let mut text = path.head.as_ref().to_string();
        for projection in &place.projections {
            let HirProjection::FieldAccess(field, _) = projection else {
                return None;
            };
            text.push('.');
            text.push_str(field.as_ref());
        }
        Some(text)
    }

    /// Whether `owner` declares a member function named `name`, concrete or
    /// generic. Probes only: nothing is reported.
    pub(super) fn declares_member_function(
        &mut self,
        id: HirId,
        span: Span,
        owner: &ResolvedType,
        name: &Ident,
    ) -> bool {
        self.without_diagnostics(|this| {
            !this
                .find_functions(id, span, owner, name, FunctionNamespace::Member)
                .is_empty()
                || matches!(
                    this.generic_method_template(
                        id,
                        span,
                        owner.autoderef(),
                        name,
                        FunctionNamespace::Member
                    ),
                    MethodTemplate::Found(_)
                )
        })
    }

    /// Written generic arguments belong to a generic declaration, which was
    /// already tried by the time a member resolves to anything else.
    fn reject_member_generic_args(
        &mut self,
        site: CallSite,
        field: &Ident,
        receiver_type: &ResolvedType,
        generic_args: &[ExprGenericArg],
    ) -> Option<()> {
        if generic_args.is_empty() {
            return Some(());
        }
        self.error(
            site.id,
            site.span,
            AnalysisErrorKind::MemberTakesNoGenericArgs {
                owner: Self::owner_name(receiver_type),
                member: field.clone(),
            },
        );
        None
    }

    /// `value.name(...)` where `name` is one of the owner's generic
    /// declarations. `None` declines, leaving the missing-name diagnostics to
    /// the ordinary path.
    ///
    /// Completing the call lives in its own function so that `?` there means
    /// "invalid, and reported" rather than "declined".
    fn resolve_generic_method_callee(
        &mut self,
        site: CallSite,
        field: &Ident,
        field_origin: Origin,
        receiver: &Receiver,
        written_generics: &[ExprGenericArg],
        args: &[HirExprNode],
        expected: Expected<'_>,
    ) -> Option<Option<CalleeResolution>> {
        let owner = receiver.r#type.autoderef().clone();
        let template = match self.generic_method_template(
            site.id,
            site.span,
            &owner,
            field,
            FunctionNamespace::Member,
        ) {
            MethodTemplate::Absent => return None,
            MethodTemplate::Failed => return Some(None),
            MethodTemplate::Found(template) => template,
        };
        Some(self.finish_generic_method_callee(
            site,
            field,
            field_origin,
            receiver,
            &owner,
            &template,
            written_generics,
            args,
            expected,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_generic_method_callee(
        &mut self,
        site: CallSite,
        field: &Ident,
        field_origin: Origin,
        receiver: &Receiver,
        owner: &ResolvedType,
        template: &GenericMethodTemplate,
        written_generics: &[ExprGenericArg],
        args: &[HirExprNode],
        expected: Expected<'_>,
    ) -> Option<CalleeResolution> {
        let declared = Self::owner_item_path(owner, field);
        let explicit = self.resolve_written_generics(
            site.id,
            site.span,
            written_generics,
            &declared,
            &template.template.generics,
        )?;
        // The receiver is supplied by the instance syntax itself, so it is
        // the one parameter neither inference nor the argument check sees.
        let (method, checked_args) = self.instantiate_generic_method_call(
            site.id,
            site.span,
            owner,
            field,
            FunctionNamespace::Member,
            template,
            &explicit,
            1,
            args,
            expected,
        )?;
        let checked_args = self.finish_generic_call_arguments(
            site.id,
            site.span,
            &method.fn_type,
            1,
            args.len(),
            checked_args,
        )?;
        self.finish_method_callee(
            site,
            field,
            field_origin,
            receiver.clone(),
            method,
            Some(checked_args),
        )
    }

    fn resolve_method_callee(
        &mut self,
        site: CallSite,
        field: &Ident,
        field_origin: Origin,
        receiver: Receiver,
        methods: Vec<ResolvedMethod>,
        args: &[HirExprNode],
    ) -> Option<CalleeResolution> {
        let (method, checked_args) = self.pick_method(site, field, methods, args)?;
        self.finish_method_callee(site, field, field_origin, receiver, method, checked_args)
    }

    fn finish_method_callee(
        &mut self,
        site: CallSite,
        field: &Ident,
        field_origin: Origin,
        receiver: Receiver,
        method: ResolvedMethod,
        checked_args: Option<Vec<CheckedExprNode>>,
    ) -> Option<CalleeResolution> {
        self.require_method_visible(site, field, field_origin, &receiver.r#type, &method)?;

        let self_mode = method
            .fn_type
            .self_mode
            .expect("a member candidate always has a self mode");
        let self_arg = self.adapt_self_argument(site, receiver, self_mode)?;

        let fn_type = ResolvedType::Function(method.fn_type.clone());
        let callee_expr = CheckedExprNode {
            id: site.id,
            span: site.span,
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
        site: CallSite,
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
            self.resolve_overload(site.id, site.span, field, &candidates, &[], args)?;
        Some((members[winner].clone(), Some(checked)))
    }

    fn owner_name(receiver_type: &ResolvedType) -> Ident {
        match receiver_type.autoderef() {
            ResolvedType::Struct(nominal) => nominal.name.clone(),
            ResolvedType::Union(nominal) => nominal.name.clone(),
            ResolvedType::Enum { nominal, .. } => nominal.name.clone(),
            other => Ident(other.to_string()),
        }
    }

    fn require_method_visible(
        &mut self,
        site: CallSite,
        field: &Ident,
        field_origin: Origin,
        receiver_type: &ResolvedType,
        method: &ResolvedMethod,
    ) -> Option<()> {
        if self.check_visibility(method.visibility, &method.declaring_module, field_origin) {
            return Some(());
        }
        self.error(
            site.id,
            site.span,
            AnalysisErrorKind::MethodNotVisible {
                method: field.clone(),
                base: method.visibility_owner(receiver_type),
            },
        );
        None
    }

    fn adapt_self_argument(
        &mut self,
        site: CallSite,
        receiver: Receiver,
        self_mode: SelfMode,
    ) -> Option<CheckedExprNode> {
        let (id, span) = (site.id, site.span);
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
                if wants_mutable {
                    self.widen_place(&place);
                }
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
                    self.widen_place(&place);
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
                let inner = match value {
                    ConstValue::Ref(inner) => *inner,
                    ConstValue::Reflected(reflected) => {
                        match crate::reflection::deref(
                            self.resolver.types(),
                            &reflected,
                            pointee,
                            self.pointer_bytes(),
                        ) {
                            Ok(inner) => inner,
                            Err(reason) => {
                                self.error(
                                    id,
                                    span,
                                    AnalysisErrorKind::CompEvalFailed {
                                        reason: reason.into(),
                                        failure: None,
                                        trace: vec![],
                                    },
                                );
                                return None;
                            }
                        }
                    }
                    _ => unreachable!(
                        "a comp value's own type is only ever Pointer alongside a ConstValue::Ref or ConstValue::Reflected"
                    ),
                };
                Some(node(pointee.widened(), CheckedExpr::Const(inner)))
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
        field_origin: Origin,
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
        let slot = &flattened[slot_index];
        let declaring_module = slot.spec.module_path.clone();
        if !self.check_visibility(slot.visibility, &declaring_module, field_origin) {
            self.error(
                id,
                span,
                AnalysisErrorKind::MethodNotVisible {
                    method: field.clone(),
                    base: ResolvedType::Spec(slot.spec.clone()),
                },
            );
            return None;
        }
        let RequirementSignature::Concrete { fn_type, .. } = &slot.signature else {
            unreachable!("generic requirements cannot reach a dynamic spec-object slot")
        };
        let fn_type = fn_type.clone();
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
        for (arg, param) in args.iter().zip(&param_types) {
            let expected_type = &param.r#type;
            let Some(checked_arg) = self.analyze_expr(arg, Expected::Exact(expected_type)) else {
                ok = false;
                continue;
            };
            let checked_arg = self.coerce_to_expected(Some(expected_type), checked_arg);
            if !Self::value_type_compatible(expected_type, &checked_arg.r#type) {
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
