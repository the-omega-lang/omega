use crate::{
    checked::{
        CheckedAddressOf, CheckedAssignment, CheckedDeclaration, CheckedExpr, CheckedExprNode,
        CheckedExternDecl, CheckedFunctionCall, CheckedFunctionDef, CheckedItem, CheckedModule,
        CheckedParam, CheckedPlace, CheckedPlaceRoot, CheckedProjection, CheckedStmt,
        CheckedStructDef, Storage,
    },
    context::{Context, VarBinding},
    error::{AnalysisError, AnalysisErrorKind},
    resolved_type::{ResolvedFunctionType, ResolvedMethod, ResolvedStructType, ResolvedType},
};
use omega_hir::{
    HirAddressOf, HirDeclaration, HirExpr, HirExprNode, HirExternDeclaration, HirFunctionDef,
    HirId, HirItem, HirModule, HirParam, HirPlace, HirPlaceRoot, HirProjection, HirStmt,
    HirStructDef,
};
use omega_parser::prelude::{Ident, SimpleSpan, Type};
use std::collections::HashSet;

/// A function-call's callee, resolved to either an ordinary value (whose
/// type must be `Function`) or a bound method reference (a "thiscall"):
/// `base.method(args)` where `method` names a struct method rather than a
/// field becomes an ordinary call to the method with `&base` (or, if `base`
/// was already a pointer, `base` itself) prepended as the first (`self`)
/// argument -- `HirFunctionDef`'s synthetic `self` parameter (see
/// `omega_hir::lower::lower_function_def`) already accounts for it in
/// `fn_type`, so no special-casing is needed in the argument-checking loop
/// in `FunctionCall` handling.
struct ResolvedCallee {
    callee: CheckedExprNode,
    fn_type: ResolvedFunctionType,
    implicit_self: Option<CheckedExprNode>,
}

#[derive(Debug, Clone)]
pub struct Analyzer {
    errors: Vec<AnalysisError>,
    context: Context,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        Self {
            errors: vec![],
            context: Context::new(),
        }
    }

    // Small generic fold used everywhere a list of HIR nodes is analyzed
    // into a list of checked ones: unlike a short-circuiting `?`/`collect`,
    // this keeps analyzing every item (so independent errors in the same
    // function/struct/module are all reported in one pass), and only
    // succeeds if every item did.
    fn analyze_all<T, U>(
        &mut self,
        items: &[T],
        mut f: impl FnMut(&mut Self, &T) -> Option<U>,
    ) -> Option<Vec<U>> {
        let mut checked = Vec::with_capacity(items.len());
        let mut ok = true;
        for item in items {
            match f(self, item) {
                Some(value) => checked.push(value),
                None => ok = false,
            }
        }
        ok.then_some(checked)
    }

    fn resolve_type_or_error(&mut self, id: HirId, span: SimpleSpan, typ: &Type) -> Option<ResolvedType> {
        match self.context.resolve_type(typ.to_owned()) {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                self.errors
                    .push(AnalysisError::new(id, span, AnalysisErrorKind::UnresolvedType(err)));
                None
            }
        }
    }

    /// Binds `ident` in the current scope, or records `Redeclaration` if
    /// it's already bound there. Centralizes what used to be, incorrectly, a
    /// codegen-side check on a name-keyed stack-slot map.
    fn declare_binding(
        &mut self,
        id: HirId,
        span: SimpleSpan,
        ident: &Ident,
        r#type: ResolvedType,
        storage: Storage,
    ) -> Option<()> {
        let binding = VarBinding { decl_id: id, storage, r#type };
        match self.context.current_scope().declare(ident.clone(), binding) {
            Ok(()) => Some(()),
            Err(dup) => {
                self.errors
                    .push(AnalysisError::new(id, span, AnalysisErrorKind::Redeclaration(dup)));
                None
            }
        }
    }

    fn analyze_declaration(&mut self, decl: &HirDeclaration, storage: Storage) -> Option<CheckedDeclaration> {
        let resolved_type = self.resolve_type_or_error(decl.id, decl.span, &decl.r#type)?;
        self.declare_binding(decl.id, decl.span, &decl.ident, resolved_type.clone(), storage)?;
        Some(CheckedDeclaration {
            id: decl.id,
            span: decl.span,
            ident: decl.ident.clone(),
            r#type: resolved_type,
        })
    }

    fn analyze_extern_decl(&mut self, extern_decl: &HirExternDeclaration) -> Option<CheckedExternDecl> {
        let resolved_type = self.resolve_type_or_error(extern_decl.id, extern_decl.span, &extern_decl.r#type)?;
        // An extern of function type imports a callable symbol; anything
        // else is extern *data*, whose storage isn't decided yet (see
        // `Storage::Global`'s doc comment).
        let storage = if matches!(resolved_type, ResolvedType::Function(_)) {
            Storage::Function
        } else {
            Storage::Global
        };
        self.declare_binding(
            extern_decl.id,
            extern_decl.span,
            &extern_decl.ident,
            resolved_type.clone(),
            storage,
        )?;
        Some(CheckedExternDecl {
            id: extern_decl.id,
            span: extern_decl.span,
            ident: extern_decl.ident.clone(),
            r#type: resolved_type,
        })
    }

    fn analyze_param(&mut self, param: &HirParam) -> Option<CheckedParam> {
        let resolved_type = self.resolve_type_or_error(param.id, param.span, &param.r#type)?;
        self.declare_binding(param.id, param.span, &param.ident, resolved_type.clone(), Storage::Parameter)?;
        Some(CheckedParam {
            id: param.id,
            span: param.span,
            ident: param.ident.clone(),
            r#type: resolved_type,
        })
    }

    /// Struct fields aren't scope-bound names (they're only ever reached
    /// through a `FieldAccess` projection off a struct-typed base), so unlike
    /// params they don't go through `declare_binding` -- but duplicate field
    /// names are still rejected, via a plain per-struct name set.
    fn analyze_struct_fields(&mut self, fields: &[HirParam]) -> Option<Vec<CheckedParam>> {
        let mut seen = HashSet::new();
        self.analyze_all(fields, |this, field| {
            if !seen.insert(field.ident.clone()) {
                this.errors.push(AnalysisError::new(
                    field.id,
                    field.span,
                    AnalysisErrorKind::Redeclaration(field.ident.clone()),
                ));
                return None;
            }
            let resolved_type = this.resolve_type_or_error(field.id, field.span, &field.r#type)?;
            Some(CheckedParam {
                id: field.id,
                span: field.span,
                ident: field.ident.clone(),
                r#type: resolved_type,
            })
        })
    }

    /// Resolves a single `.field` step against `current_type`, inserting a
    /// seamless one-level pointer deref first if needed (`ptr.field` is
    /// sugar for `(*ptr).field` when `ptr` is a pointer-to-struct, matching
    /// Rust's autoderef -- exactly one level: `ptr.field` where `ptr` is
    /// `**Struct` still needs an explicit `(*ptr).field`). Shared by
    /// `analyze_place`'s projection loop and member-call resolution below,
    /// so both plain field access and method access get this for free from
    /// one implementation.
    fn resolve_field_projection(
        &mut self,
        node_id: HirId,
        span: SimpleSpan,
        projections: &mut Vec<CheckedProjection>,
        current_type: &ResolvedType,
        field: &Ident,
    ) -> Option<ResolvedType> {
        let dereffed = match current_type {
            ResolvedType::Pointer(inner) => {
                projections.push(CheckedProjection::Deref { r#type: (**inner).clone() });
                (**inner).clone()
            }
            other => other.clone(),
        };

        let ResolvedType::Struct(struct_type) = &dereffed else {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAStruct));
            return None;
        };

        let found = struct_type
            .fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == field)
            .map(|(index, (_, r#type))| (index, r#type.clone()));
        let Some((index, field_type)) = found else {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NoSuchField(field.clone())));
            return None;
        };

        projections.push(CheckedProjection::FieldAccess {
            field: field.clone(),
            index,
            r#type: field_type.clone(),
        });
        Some(field_type)
    }

    /// Read-only peek at whether `field`, applied to `current_type` (after
    /// the same up-to-one-level seamless deref `resolve_field_projection`
    /// would apply), names a struct method rather than a field -- used to
    /// detect a member call (`base.method(args)`) before committing to
    /// resolving `field` as an ordinary field access. A field with this name
    /// always shadows a method with the same name.
    fn find_method(&self, current_type: &ResolvedType, field: &Ident) -> Option<ResolvedMethod> {
        let dereffed = match current_type {
            ResolvedType::Pointer(inner) => inner.as_ref(),
            other => other,
        };
        let ResolvedType::Struct(struct_type) = dereffed else {
            return None;
        };
        if struct_type.fields.iter().any(|(name, _)| name == field) {
            return None;
        }
        struct_type
            .functions
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, method)| method.clone())
    }

    /// Resolves a place's root, then folds over its projections in source
    /// order, resolving field/index/deref projections against the running
    /// type and recording the exact resolved shape (field index, item/
    /// pointee type) so codegen never has to re-search or re-derive them.
    fn analyze_place(
        &mut self,
        node_id: HirId,
        span: SimpleSpan,
        place: &HirPlace,
    ) -> Option<(CheckedPlace, ResolvedType)> {
        let (root, mut current_type) = match &place.root {
            HirPlaceRoot::Ident(ident) => {
                let Some(binding) = self.context.find_variable(ident) else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UndefinedVariable(ident.clone()),
                    ));
                    return None;
                };
                let root = CheckedPlaceRoot::Variable {
                    decl_id: binding.decl_id,
                    storage: binding.storage,
                    r#type: binding.r#type.clone(),
                };
                (root, binding.r#type.clone())
            }
            HirPlaceRoot::Expr(expr) => {
                let checked_expr = self.analyze_expr(expr)?;
                let r#type = checked_expr.r#type.clone();
                (CheckedPlaceRoot::Expr(Box::new(checked_expr)), r#type)
            }
        };

        let mut projections = Vec::with_capacity(place.projections.len());
        for projection in &place.projections {
            match projection {
                HirProjection::FieldAccess(field) => {
                    current_type =
                        self.resolve_field_projection(node_id, span, &mut projections, &current_type, field)?;
                }
                HirProjection::Index(index_expr) => {
                    let checked_index = self.analyze_expr(index_expr)?;
                    let ResolvedType::Array(item_type) = current_type else {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAnArray));
                        return None;
                    };
                    let item_type = *item_type;
                    projections.push(CheckedProjection::Index {
                        index_expr: Box::new(checked_index),
                        item_type: item_type.clone(),
                    });
                    current_type = item_type;
                }
                HirProjection::Deref => {
                    let ResolvedType::Pointer(inner) = current_type else {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAPointer));
                        return None;
                    };
                    let inner_type = *inner;
                    projections.push(CheckedProjection::Deref { r#type: inner_type.clone() });
                    current_type = inner_type;
                }
            }
        }

        Some((CheckedPlace { root, projections }, current_type))
    }

    fn resolve_callee(&mut self, callee: &HirExprNode) -> Option<ResolvedCallee> {
        if let HirExpr::Place(place) = &callee.expr
            && let Some(HirProjection::FieldAccess(field)) = place.projections.last()
        {
            let base_place = HirPlace {
                root: place.root.clone(),
                projections: place.projections[..place.projections.len() - 1].to_vec(),
            };
            let (checked_base, base_type) = self.analyze_place(callee.id, callee.span, &base_place)?;

            if let Some(method) = self.find_method(&base_type, field) {
                // `self` is `&base` -- or, if `base` is already a pointer,
                // `base` itself (that's exactly what a seamless deref would
                // have produced, so there's no need to materialize a
                // Deref-then-AddressOf round trip just to get back the same
                // pointer value).
                let self_arg = if matches!(base_type, ResolvedType::Pointer(_)) {
                    CheckedExprNode {
                        id: callee.id,
                        span: callee.span,
                        r#type: base_type,
                        kind: CheckedExpr::Place(checked_base),
                    }
                } else {
                    let pointer_type = ResolvedType::Pointer(Box::new(base_type));
                    CheckedExprNode {
                        id: callee.id,
                        span: callee.span,
                        r#type: pointer_type,
                        kind: CheckedExpr::AddressOf(CheckedAddressOf { place: checked_base }),
                    }
                };

                let callee_expr = CheckedExprNode {
                    id: callee.id,
                    span: callee.span,
                    r#type: ResolvedType::Function(method.fn_type.clone()),
                    kind: CheckedExpr::Place(CheckedPlace {
                        root: CheckedPlaceRoot::Variable {
                            decl_id: method.decl_id,
                            storage: Storage::Function,
                            r#type: ResolvedType::Function(method.fn_type.clone()),
                        },
                        projections: vec![],
                    }),
                };

                return Some(ResolvedCallee {
                    callee: callee_expr,
                    fn_type: method.fn_type,
                    implicit_self: Some(self_arg),
                });
            }

            // Not a method -- finish resolving the ordinary field access
            // using the base place we already have, instead of re-resolving
            // the whole place from scratch (which would risk reporting the
            // base's errors, e.g. an undefined variable, twice).
            let CheckedPlace { root, mut projections } = checked_base;
            let field_type =
                self.resolve_field_projection(callee.id, callee.span, &mut projections, &base_type, field)?;
            let checked_callee = CheckedExprNode {
                id: callee.id,
                span: callee.span,
                r#type: field_type.clone(),
                kind: CheckedExpr::Place(CheckedPlace { root, projections }),
            };
            let ResolvedType::Function(fn_type) = field_type else {
                self.errors
                    .push(AnalysisError::new(callee.id, callee.span, AnalysisErrorKind::UnresolvedCallee));
                return None;
            };
            return Some(ResolvedCallee { callee: checked_callee, fn_type, implicit_self: None });
        }

        let checked_callee = self.analyze_expr(callee)?;
        let ResolvedType::Function(fn_type) = checked_callee.r#type.clone() else {
            self.errors
                .push(AnalysisError::new(callee.id, callee.span, AnalysisErrorKind::UnresolvedCallee));
            return None;
        };
        Some(ResolvedCallee { callee: checked_callee, fn_type, implicit_self: None })
    }

    fn analyze_expr(&mut self, node: &HirExprNode) -> Option<CheckedExprNode> {
        let node_id = node.id;
        let span = node.span;

        match &node.expr {
            HirExpr::Place(place) => {
                let (checked_place, r#type) = self.analyze_place(node_id, span, place)?;
                Some(CheckedExprNode { id: node_id, span, r#type, kind: CheckedExpr::Place(checked_place) })
            }

            HirExpr::Number(number_expr) => {
                let resolved_type = match &number_expr.explicit_type {
                    Some(explicit_type) => {
                        // Only `i32` is a supported numeric type today (see
                        // the parser's own "TODO: handle floats and unsigned
                        // integers"); anything else -- unrecognized, or
                        // recognized but non-numeric -- is invalid here.
                        match self.context.resolve_type(Type::Named(explicit_type.clone())) {
                            Ok(ResolvedType::I32) => ResolvedType::I32,
                            _ => {
                                self.errors.push(AnalysisError::new(
                                    node_id,
                                    span,
                                    AnalysisErrorKind::InvalidNumberType(explicit_type.clone()),
                                ));
                                return None;
                            }
                        }
                    }
                    None => ResolvedType::I32,
                };

                let Ok(value) = number_expr.integer_part.parse::<i32>() else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::NumberLiteralOutOfRange {
                            literal: number_expr.integer_part.clone(),
                        },
                    ));
                    return None;
                };

                Some(CheckedExprNode { id: node_id, span, r#type: resolved_type, kind: CheckedExpr::Number(value) })
            }

            HirExpr::String(s) => Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: ResolvedType::Pointer(Box::new(ResolvedType::Char)),
                kind: CheckedExpr::String(s.0.clone()),
            }),

            HirExpr::Codeblock(stmts) => {
                self.context.enter_scope();
                let checked_stmts = self.analyze_stmts(stmts);
                self.context.leave_scope();
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Void,
                    kind: CheckedExpr::Codeblock(checked_stmts?),
                })
            }

            HirExpr::FunctionCall(call) => {
                let ResolvedCallee { callee, fn_type, implicit_self } = self.resolve_callee(&call.callee)?;

                let mut args = Vec::with_capacity(call.args.len() + implicit_self.is_some() as usize);
                args.extend(implicit_self);

                for arg in &call.args {
                    let param_index = args.len();
                    if param_index >= fn_type.params.len() && !fn_type.is_variadic {
                        self.errors.push(AnalysisError::new(
                            arg.id,
                            arg.span,
                            AnalysisErrorKind::TooManyArguments { expected: fn_type.params.len() },
                        ));
                        return None;
                    }

                    let checked_arg = self.analyze_expr(arg)?;

                    if param_index < fn_type.params.len() {
                        let expected_type = &fn_type.params[param_index].1;
                        if &checked_arg.r#type != expected_type {
                            self.errors.push(AnalysisError::new(
                                arg.id,
                                arg.span,
                                AnalysisErrorKind::ArgumentTypeMismatch {
                                    expected: expected_type.clone(),
                                    found: checked_arg.r#type.clone(),
                                },
                            ));
                            return None;
                        }
                    }

                    args.push(checked_arg);
                }

                let return_type = *fn_type.return_type.clone();
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: return_type,
                    kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee), fn_type, args }),
                })
            }

            HirExpr::Assignment(assignment) => {
                let checked_value = self.analyze_expr(&assignment.value)?;

                let HirExpr::Place(place) = &assignment.target.expr else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::AssignmentTargetNotAPlace,
                    ));
                    return None;
                };
                let (checked_target, target_type) =
                    self.analyze_place(assignment.target.id, assignment.target.span, place)?;

                if target_type != checked_value.r#type {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::AssignmentTypeMismatch {
                            target: target_type,
                            value: checked_value.r#type,
                        },
                    ));
                    return None;
                }

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: target_type,
                    kind: CheckedExpr::Assignment(CheckedAssignment {
                        target: checked_target,
                        value: Box::new(checked_value),
                    }),
                })
            }

            HirExpr::AddressOf(HirAddressOf { base }) => {
                let HirExpr::Place(place) = &base.expr else {
                    self.errors
                        .push(AnalysisError::new(node_id, span, AnalysisErrorKind::AddressOfNotAPlace));
                    return None;
                };
                let (checked_place, place_type) = self.analyze_place(base.id, base.span, place)?;

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Pointer(Box::new(place_type)),
                    kind: CheckedExpr::AddressOf(CheckedAddressOf { place: checked_place }),
                })
            }
        }
    }

    fn analyze_stmt(&mut self, stmt: &HirStmt) -> Option<CheckedStmt> {
        match stmt {
            HirStmt::Declaration(decl) => self.analyze_declaration(decl, Storage::Local).map(CheckedStmt::Declaration),
            HirStmt::ExternDeclaration(decl) => self.analyze_extern_decl(decl).map(CheckedStmt::ExternDeclaration),
            HirStmt::Expression(expr) => self.analyze_expr(expr).map(CheckedStmt::Expression),
            HirStmt::Return(expr) => self.analyze_expr(expr).map(CheckedStmt::Return),
            HirStmt::Struct(struct_def) => self.analyze_struct_def(struct_def).map(CheckedStmt::Struct),
        }
    }

    fn analyze_stmts(&mut self, stmts: &[HirStmt]) -> Option<Vec<CheckedStmt>> {
        self.analyze_all(stmts, Self::analyze_stmt)
    }

    fn analyze_function_def(&mut self, f: &HirFunctionDef) -> Option<CheckedFunctionDef> {
        self.context.enter_scope();
        let params = self.analyze_all(&f.params, Self::analyze_param);
        let return_type = self.resolve_type_or_error(f.id, f.span, &f.return_type);
        let body = self.analyze_stmts(&f.body);
        self.context.leave_scope();

        let checked = CheckedFunctionDef {
            id: f.id,
            span: f.span,
            name: f.name.clone(),
            is_member_function: f.is_member_function,
            is_variadic: false,
            params: params?,
            return_type: return_type?,
            body: body?,
        };

        // Register the function's own name in whatever scope is current now
        // that its body scope has been popped -- the enclosing module scope
        // for a top-level function, or the struct's dedicated method scope
        // for a member function (see `analyze_struct_def`) -- so later
        // top-level items, or sibling methods analyzed after this one, can
        // call it by name.
        let binding = VarBinding {
            decl_id: f.id,
            storage: Storage::Function,
            r#type: ResolvedType::Function(checked.fn_type()),
        };
        if let Err(dup) = self.context.current_scope().declare(f.name.clone(), binding) {
            self.errors
                .push(AnalysisError::new(f.id, f.span, AnalysisErrorKind::Redeclaration(dup)));
            return None;
        }

        Some(checked)
    }

    fn analyze_struct_def(&mut self, s: &HirStructDef) -> Option<CheckedStructDef> {
        let fields = self.analyze_struct_fields(&s.fields)?;

        // Insert the struct's type -- with an empty method list for now --
        // *before* analyzing any method, since a member function's synthetic
        // `self: *StructName` parameter needs the struct's own name to
        // already resolve.
        // TODO: Make sure type does not already exist
        self.context.current_scope().defined_types.insert(
            s.name.clone(),
            ResolvedType::Struct(ResolvedStructType {
                fields: fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect(),
                functions: vec![],
            }),
        );

        // Methods are bound in their own nested scope so they aren't
        // globally callable; `resolve_type` still sees the struct's type
        // just inserted above by walking outward through the scope stack.
        self.context.enter_scope();
        let functions = self.analyze_all(&s.functions, Self::analyze_function_def);
        self.context.leave_scope();
        let functions = functions?;

        // Back in the exact scope frame the struct's type was inserted into
        // above (the enter/leave pair around the methods loop brackets
        // symmetrically) -- patch in the now-resolved method list directly,
        // no parent-scope depth arithmetic required.
        let ResolvedType::Struct(resolved) = self
            .context
            .current_scope()
            .defined_types
            .get_mut(&s.name)
            .expect("just inserted above, in this exact scope frame")
        else {
            unreachable!("just inserted as ResolvedType::Struct above");
        };
        resolved.functions = functions
            .iter()
            .map(|f| (f.name.clone(), ResolvedMethod { decl_id: f.id, fn_type: f.fn_type() }))
            .collect();

        Some(CheckedStructDef { id: s.id, span: s.span, name: s.name.clone(), fields, functions })
    }

    fn analyze_item(&mut self, item: &HirItem) -> Option<CheckedItem> {
        match item {
            HirItem::Declaration(decl) => self.analyze_declaration(decl, Storage::Global).map(CheckedItem::Declaration),
            HirItem::ExternDeclaration(decl) => self.analyze_extern_decl(decl).map(CheckedItem::ExternDeclaration),
            HirItem::FunctionDefinition(f) => self.analyze_function_def(f).map(CheckedItem::FunctionDefinition),
            HirItem::Struct(s) => self.analyze_struct_def(s).map(CheckedItem::Struct),
        }
    }

    pub fn analyze(mut self, hir_module: &HirModule) -> Result<CheckedModule, Vec<AnalysisError>> {
        let items = self.analyze_all(&hir_module.items, Self::analyze_item);

        match items {
            Some(items) if self.errors.is_empty() => Ok(CheckedModule { id: hir_module.id, items }),
            _ => Err(self.errors),
        }
    }
}

