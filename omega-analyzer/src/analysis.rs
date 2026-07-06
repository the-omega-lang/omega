use crate::{
    checked::{
        CheckedAddressOf, CheckedArrayLiteral, CheckedAssignment, CheckedBinaryOp, CheckedBlock,
        CheckedBreak, CheckedContinue, CheckedDeclaration, CheckedExpr, CheckedExprNode,
        CheckedExternDecl, CheckedFor, CheckedFunctionCall, CheckedFunctionDef, CheckedIf,
        CheckedItem, CheckedModule, CheckedParam, CheckedPlace, CheckedPlaceRoot,
        CheckedProjection, CheckedSlice, CheckedStmt, CheckedStructDef, CheckedWhile, NumberValue,
        Storage,
    },
    context::{Context, VarBinding},
    error::{AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind},
    resolved_type::{NumericKind, ResolvedFunctionType, ResolvedMethod, ResolvedStructType, ResolvedType},
    resolver::{ImportTarget, ModuleResolver, ModuleSignature, ResolvedItem, SignatureEntry, Visibility},
};
use omega_hir::{
    BinaryOp, HirAddressOf, HirBlock, HirDeclaration, HirExpr, HirExprNode, HirExternDeclaration,
    HirFor, HirFunctionDef, HirId, HirIf, HirImport, HirItem, HirModule, HirParam, HirPlace,
    HirPlaceRoot, HirProjection, HirSlice, HirStmt, HirStructDef, HirWalrusDeclaration,
};
use omega_parser::prelude::{Ident, NumberBase, NumberExpr, SimpleSpan, Type};
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

pub struct Analyzer<'r> {
    errors: Vec<AnalysisError>,
    /// Non-fatal findings -- currently just unreachable code (see
    /// `truncate_unreachable`) -- returned alongside a successful
    /// `CheckedModule` rather than folded into `errors`, since none of them
    /// reject the program. See `AnalysisWarning`'s doc comment.
    warnings: Vec<AnalysisWarning>,
    context: Context,
    /// Everything module-tree-shaped -- filesystem lookups, cross-module
    /// caching, cycle detection -- lives entirely on the other side of this
    /// trait object (see `crate::resolver`); the same long-lived resolver
    /// (the driver) is borrowed across many short-lived per-module
    /// `Analyzer`s, one per `collect_signatures`/`analyze_bodies` call,
    /// rather than owned by any one of them.
    resolver: &'r mut dyn ModuleResolver,
    /// The enclosing function's declared return type, checked against every
    /// `return <expr>;` and against the function body's own effective type
    /// (see `block_type`/`check_function_return`). Saved and restored around
    /// each `analyze_function_def` call (rather than set once) since a
    /// struct -- and therefore its methods -- can be declared inside a
    /// function body, nesting one function's analysis inside another's.
    current_return_type: ResolvedType,
    /// A stack of enclosing loops' `HirId`s (innermost last), pushed/popped
    /// around a `while`/`for`'s body analysis. `break`/`continue` resolve
    /// against this -- today always `.last()` (the innermost loop), but
    /// looked up rather than hard-assumed specifically so a future labeled
    /// `break 'outer;` only has to change *this* resolution rule (search the
    /// stack for a matching label instead of always taking the top); nothing
    /// about `HirBreak`/`CheckedBreak`/codegen would need to change. Saved
    /// and restored around each `analyze_function_def` call, same reasoning
    /// as `current_return_type`: a loop body can't leak into a struct method
    /// declared inside it.
    loop_stack: Vec<HirId>,
}

impl<'r> Analyzer<'r> {
    pub fn new(resolver: &'r mut dyn ModuleResolver) -> Self {
        Self {
            errors: vec![],
            warnings: vec![],
            context: Context::new(),
            resolver,
            current_return_type: ResolvedType::Void,
            loop_stack: vec![],
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
        match self.context.resolve_type(typ.to_owned(), &mut *self.resolver) {
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

        // `slice.length` -- not a real field (a slice isn't a `Struct`), so
        // this is checked before the struct-only path below rejects it. Any
        // other field name on a slice is simply `NoSuchField`, same message a
        // struct without that field would give.
        if let ResolvedType::Slice(_) = &dereffed {
            if field.as_ref() == "length" {
                projections.push(CheckedProjection::SliceLength);
                return Some(ResolvedType::I32);
            }
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NoSuchField(field.clone())));
            return None;
        }

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
            HirPlaceRoot::Path(path) if path.is_unqualified() => {
                let ident = &path.head;
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
            // A qualified place root (`mymodule::thing::foo`) -- `path`'s
            // head must already be an imported module alias (requirement 7:
            // nothing is visible across modules without an explicit
            // `import`); the rest is resolved across modules by `resolver`.
            HirPlaceRoot::Path(path) => {
                let Some(absolute) = self.context.absolute_path(path) else {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UndefinedVariable(path.head.clone()),
                    ));
                    return None;
                };
                match self.resolver.resolve_item(&absolute) {
                    Ok(ResolvedItem::Value { r#type, storage, decl_id }) => {
                        let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                        (root, r#type)
                    }
                    Ok(ResolvedItem::Type(_)) => {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAValue(absolute)));
                        return None;
                    }
                    Err(e) => {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                        return None;
                    }
                }
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
                    // `Array` (the legacy thin-pointer unsized form, e.g.
                    // `argv`), `SizedArray`, and `Slice` are all indexable by
                    // a single element -- codegen tells them apart itself
                    // (see `resolve_place_storage`'s `Index` arm) using the
                    // exact same `current_type` this match is on.
                    let item_type = match current_type {
                        ResolvedType::Array(item_type) => *item_type,
                        ResolvedType::SizedArray(item_type, _) => *item_type,
                        ResolvedType::Slice(item_type) => *item_type,
                        _ => {
                            self.errors
                                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAnArray));
                            return None;
                        }
                    };
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

    /// Resolves a number literal's target type (explicit suffix, or the
    /// default -- `f64` for a literal with a decimal point, `i32` otherwise,
    /// mirroring Rust's own literal defaults) and parses/range-checks its
    /// text against that type. `NumberExpr` keeps its digits as plain text
    /// (see its doc comment) precisely so this is the *only* place that ever
    /// has to interpret them -- codegen just emits whatever `NumberValue`
    /// this produces.
    fn analyze_number(&mut self, node_id: HirId, span: SimpleSpan, n: &NumberExpr) -> Option<CheckedExprNode> {
        let invalid_suffix = |this: &mut Self, ident: &Ident| {
            this.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::InvalidNumberType(ident.clone())));
        };

        let resolved_type = match &n.explicit_type {
            Some(explicit_type) => match self
                .context
                .resolve_type(Type::Named(explicit_type.clone().into()), &mut *self.resolver)
            {
                Ok(r#type) if r#type.numeric_kind().is_some() => r#type,
                _ => {
                    invalid_suffix(self, explicit_type);
                    return None;
                }
            },
            None if n.fractional_part.is_some() => ResolvedType::F64,
            None => ResolvedType::I32,
        };
        let kind = resolved_type
            .numeric_kind()
            .expect("just resolved above, or a hardcoded numeric default");

        // A literal written with a decimal point must resolve to a float
        // type; a based (hex/octal/binary) literal never carries one (the
        // grammar has no notation for it), so a float suffix on one (e.g.
        // `0xFFf32`) is rejected here too rather than silently misparsed.
        let is_float = matches!(kind, NumericKind::Float(_));
        if n.fractional_part.is_some() && !is_float {
            let Some(explicit_type) = &n.explicit_type else {
                unreachable!("the default type for a fractional literal is always F64");
            };
            invalid_suffix(self, explicit_type);
            return None;
        }
        if is_float && n.base != NumberBase::Decimal {
            let Some(explicit_type) = &n.explicit_type else {
                unreachable!("the default type is only Float when a fraction was written, which implies Decimal");
            };
            invalid_suffix(self, explicit_type);
            return None;
        }

        let literal_text = || match &n.fractional_part {
            Some(frac) => format!("{}.{}", n.integer_part, frac),
            None => n.integer_part.clone(),
        };
        let out_of_range = |this: &mut Self| {
            this.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::NumberLiteralOutOfRange { literal: literal_text() },
            ));
        };

        let value = match kind {
            NumericKind::Float(width) => {
                let text = format!("{}.{}", n.integer_part, n.fractional_part.as_deref().unwrap_or("0"));
                let Ok(parsed) = text.parse::<f64>() else {
                    out_of_range(self);
                    return None;
                };
                if width == 32 && parsed.is_finite() && (parsed as f32).is_infinite() {
                    out_of_range(self);
                    return None;
                }
                NumberValue::Float(parsed)
            }
            NumericKind::Signed(width) => {
                let Ok(parsed) = u64::from_str_radix(&n.integer_part, n.base.radix()) else {
                    out_of_range(self);
                    return None;
                };
                let max = if width == 64 { i64::MAX as u64 } else { (1u64 << (width - 1)) - 1 };
                if parsed > max {
                    out_of_range(self);
                    return None;
                }
                NumberValue::Signed(parsed as i64)
            }
            NumericKind::Unsigned(width) => {
                let Ok(parsed) = u64::from_str_radix(&n.integer_part, n.base.radix()) else {
                    out_of_range(self);
                    return None;
                };
                let max = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
                if parsed > max {
                    out_of_range(self);
                    return None;
                }
                NumberValue::Unsigned(parsed)
            }
        };

        Some(CheckedExprNode { id: node_id, span, r#type: resolved_type, kind: CheckedExpr::Number(value) })
    }

    /// A block's own effective type: its tail expression's type, or -- if it
    /// has none -- `Void`, *unless* its last statement unconditionally
    /// diverges (see `stmt_diverges`), in which case the block itself never
    /// actually produces `Void` at its own position (control leaves the
    /// function entirely) -- so it's exempt from whatever type is expected
    /// there, the same way Rust's `!` (never) type unifies with anything.
    /// `None` here means exactly that: "diverges, no constraint," not "has
    /// no type."
    fn block_type(block: &CheckedBlock) -> Option<ResolvedType> {
        match &block.tail {
            Some(tail) if Self::expr_diverges(tail) => None,
            Some(tail) => Some(tail.r#type.clone()),
            None => match block.stmts.last() {
                Some(stmt) if Self::stmt_diverges(stmt) => None,
                _ => Some(ResolvedType::Void),
            },
        }
    }

    /// Whether an expression unconditionally diverges: only an `if`/`else
    /// if`/`else` can (with a genuine `else`, not an implicit empty one)
    /// where *every* branch diverges -- everything else either can't
    /// diverge at all, or (a bare `return`) isn't an expression to begin
    /// with. Needed because such an `if` still gets a concrete (if
    /// degenerate, `Void`) `r#type` of its own during analysis -- there's no
    /// real "never" `ResolvedType` to give it instead -- so whether *it*
    /// diverges has to be re-derived structurally here rather than read off
    /// its `r#type`, the same way `stmt_diverges` re-derives it for a bare
    /// `return` statement.
    fn expr_diverges(expr: &CheckedExprNode) -> bool {
        match &expr.kind {
            CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                let Some(else_branch) = else_branch else { return false };
                branches.iter().all(|(_, b)| Self::block_type(b).is_none())
                    && Self::block_type(else_branch).is_none()
            }
            _ => false,
        }
    }

    /// Whether a statement unconditionally diverges (its block never
    /// actually reaches whatever position it's in): a plain `return`/
    /// `break`/`continue`, or an expression-statement that diverges (see
    /// `expr_diverges` -- currently only a fully-diverging `if`). This is
    /// still a purely syntactic check, not real reachability analysis (e.g.
    /// a `while true { return 1; }` with no way out isn't recognized as
    /// diverging), but "dispatch on a condition and return/break/continue
    /// from every arm" is common enough to be worth recognizing specifically
    /// (see `Codegen::emit_if`'s matching `BlockOutcome::Diverged`
    /// propagation, which this must stay in sync with -- codegen already
    /// builds sound IR for exactly this case).
    fn stmt_diverges(stmt: &CheckedStmt) -> bool {
        match stmt {
            CheckedStmt::Return(_) | CheckedStmt::Break(_) | CheckedStmt::Continue(_) => true,
            CheckedStmt::Expression(expr) => Self::expr_diverges(expr),
            _ => false,
        }
    }

    /// Every `CheckedStmt` variant's id/span, for anchoring an
    /// `AnalysisWarningKind::UnreachableCode` at whichever statement turns
    /// out to be first made unreachable by a diverging predecessor (see
    /// `truncate_unreachable`).
    fn checked_stmt_id_span(stmt: &CheckedStmt) -> (HirId, SimpleSpan) {
        match stmt {
            CheckedStmt::Declaration(d) => (d.id, d.span),
            CheckedStmt::ExternDeclaration(d) => (d.id, d.span),
            CheckedStmt::Expression(e) => (e.id, e.span),
            CheckedStmt::Return(e) => (e.id, e.span),
            CheckedStmt::Struct(s) => (s.id, s.span),
            CheckedStmt::While(w) => (w.id, w.span),
            CheckedStmt::For(f) => (f.id, f.span),
            CheckedStmt::Break(b) => (b.id, b.span),
            CheckedStmt::Continue(c) => (c.id, c.span),
        }
    }

    /// Drops every statement after the first one that unconditionally
    /// diverges (see `stmt_diverges`) -- they can never run, and keeping them
    /// in the `CheckedBlock` would make codegen try to emit instructions
    /// into an already-terminated cranelift block (a compiler panic, not a
    /// user-facing error; see `Codegen::emit_block`). Recorded as an
    /// `AnalysisWarningKind::UnreachableCode` rather than an `AnalysisError`:
    /// unlike everything else this pass rejects, dead code doesn't make the
    /// program incorrect, just wasteful -- the same reason real compilers
    /// warn about it instead of refusing to build.
    fn truncate_unreachable(&mut self, mut stmts: Vec<CheckedStmt>) -> Vec<CheckedStmt> {
        let Some(cutoff) = stmts.iter().position(Self::stmt_diverges) else {
            return stmts;
        };
        if let Some(first_dead) = stmts.get(cutoff + 1) {
            let (id, span) = Self::checked_stmt_id_span(first_dead);
            self.warnings.push(AnalysisWarning::new(id, span, AnalysisWarningKind::UnreachableCode));
        }
        stmts.truncate(cutoff + 1);
        stmts
    }

    /// Analyzes a `{ stmts... tail }` block in its own nested scope --
    /// shared by bare codeblock expressions, `if`/`while`/`for` bodies, and
    /// function bodies. Scope is always entered/left even on failure (before
    /// the `?` that can early-return), so an error partway through a block
    /// never leaves the scope stack unbalanced.
    fn analyze_block(&mut self, block: &HirBlock) -> Option<CheckedBlock> {
        self.context.enter_scope();
        let checked_stmts = self.analyze_stmts(&block.stmts);
        let checked_tail = block.tail.as_ref().map(|e| self.analyze_expr(e));
        self.context.leave_scope();

        let stmts = checked_stmts?;
        let tail = match checked_tail {
            Some(t) => Some(Box::new(t?)),
            None => None,
        };

        // `analyze_stmts` already truncated (and warned about) unreachable
        // statements *within* `stmts`; if what's left still ends in
        // something that diverges, a tail expression after it -- if any --
        // is unreachable too, for the same reason.
        let tail = match &tail {
            Some(t) if stmts.last().is_some_and(Self::stmt_diverges) => {
                self.warnings.push(AnalysisWarning::new(t.id, t.span, AnalysisWarningKind::UnreachableCode));
                None
            }
            _ => tail,
        };

        Some(CheckedBlock { stmts, tail })
    }

    /// `++base`/`--base`: validates `base` is a place (like `AddressOf`) of
    /// a numeric type, then desugars directly into `base = base <op> 1` --
    /// an ordinary `Assignment` wrapping a `BinaryOp` over `base`'s own
    /// place and a `Number` matching its exact resolved type/kind. Building
    /// the literal `1` here, rather than going through the parser's
    /// `NumberExpr`/`HirExpr::Number` path, is what lets this work for any
    /// numeric type (an untyped `1` in source would default to `i32`, which
    /// would then fail `BinaryOp`'s "operands must match exactly" rule for
    /// every other numeric type) -- analysis already knows `base`'s exact
    /// type here, so it can build a same-typed constant directly.
    fn analyze_incr_decr(&mut self, node_id: HirId, span: SimpleSpan, base: &HirExprNode, op: BinaryOp) -> Option<CheckedExprNode> {
        let HirExpr::Place(place) = &base.expr else {
            self.errors
                .push(AnalysisError::new(node_id, span, AnalysisErrorKind::IncrementTargetNotAPlace));
            return None;
        };
        let (checked_place, place_type) = self.analyze_place(base.id, base.span, place)?;

        let Some(kind) = place_type.numeric_kind() else {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::InvalidIncrementOperand { r#type: place_type },
            ));
            return None;
        };

        let one = match kind {
            NumericKind::Signed(_) => NumberValue::Signed(1),
            NumericKind::Unsigned(_) => NumberValue::Unsigned(1),
            NumericKind::Float(_) => NumberValue::Float(1.0),
        };
        let one_node = CheckedExprNode { id: node_id, span, r#type: place_type.clone(), kind: CheckedExpr::Number(one) };
        let place_read = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::Place(checked_place.clone()),
        };
        let sum = CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type.clone(),
            kind: CheckedExpr::BinaryOp(CheckedBinaryOp { op, left: Box::new(place_read), right: Box::new(one_node) }),
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: place_type,
            kind: CheckedExpr::Assignment(CheckedAssignment { target: checked_place, value: Box::new(sum) }),
        })
    }

    fn analyze_expr(&mut self, node: &HirExprNode) -> Option<CheckedExprNode> {
        let node_id = node.id;
        let span = node.span;

        match &node.expr {
            HirExpr::Place(place) => {
                let (checked_place, r#type) = self.analyze_place(node_id, span, place)?;
                Some(CheckedExprNode { id: node_id, span, r#type, kind: CheckedExpr::Place(checked_place) })
            }

            HirExpr::Number(number_expr) => self.analyze_number(node_id, span, number_expr),

            HirExpr::Bool(b) => {
                Some(CheckedExprNode { id: node_id, span, r#type: ResolvedType::Bool, kind: CheckedExpr::Bool(*b) })
            }

            HirExpr::Char(c) => {
                Some(CheckedExprNode { id: node_id, span, r#type: ResolvedType::Char, kind: CheckedExpr::Char(*c) })
            }

            // A string literal's bytes are raw UTF-8 bytes, not decoded
            // characters -- `*u8`, not `*char` (see `ResolvedType::Char`'s
            // doc comment), the same type C's own string literals decay to.
            HirExpr::String(s) => Some(CheckedExprNode {
                id: node_id,
                span,
                r#type: ResolvedType::Pointer(Box::new(ResolvedType::U8)),
                kind: CheckedExpr::String(s.0.clone()),
            }),

            HirExpr::Codeblock(block) => {
                let checked_block = self.analyze_block(block)?;
                let r#type = Self::block_type(&checked_block).unwrap_or(ResolvedType::Void);
                Some(CheckedExprNode { id: node_id, span, r#type, kind: CheckedExpr::Codeblock(checked_block) })
            }

            HirExpr::If(HirIf { branches, else_branch }) => {
                let mut checked_branches = Vec::with_capacity(branches.len());
                for (cond, block) in branches {
                    let checked_cond = self.analyze_expr(cond)?;
                    if checked_cond.r#type != ResolvedType::Bool {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            span,
                            AnalysisErrorKind::NonBoolCondition { r#type: checked_cond.r#type },
                        ));
                        return None;
                    }
                    let checked_block = self.analyze_block(block)?;
                    checked_branches.push((checked_cond, checked_block));
                }
                let checked_else = match else_branch {
                    Some(b) => Some(self.analyze_block(b)?),
                    None => None,
                };

                // What the whole `if` resolves to: the first concrete
                // (non-diverging) type among the branches and the `else`,
                // if any -- diverging branches (ending in `return`) are
                // wildcards, exempt from the check below. No `else` at all
                // forces `Void` (the "implicit else" is `{}`), matching
                // Rust's identical rule for a possibly-skipped `if`.
                let branch_kinds: Vec<Option<ResolvedType>> =
                    checked_branches.iter().map(|(_, b)| Self::block_type(b)).collect();
                let else_kind: Option<Option<ResolvedType>> = checked_else.as_ref().map(Self::block_type);

                let result_type = match &else_kind {
                    Some(k) => branch_kinds.iter().cloned().chain(std::iter::once(k.clone())).flatten().next(),
                    None => None,
                }
                .unwrap_or(ResolvedType::Void);

                let mismatch = branch_kinds
                    .iter()
                    .cloned()
                    .chain(else_kind.iter().cloned())
                    .flatten()
                    .find(|t| *t != result_type);
                if let Some(found) = mismatch {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::IfBranchTypeMismatch { expected: result_type, found },
                    ));
                    return None;
                }

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: result_type,
                    kind: CheckedExpr::If(CheckedIf { branches: checked_branches, else_branch: checked_else }),
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

            HirExpr::Negate(base) => {
                let checked_base = self.analyze_expr(base)?;
                // Signed ints and floats only -- matching Rust, unary `-` on
                // an unsigned integer (or `bool`/`char`, neither of which is
                // numeric at all) is rejected rather than silently wrapping.
                let negatable = matches!(
                    checked_base.r#type.numeric_kind(),
                    Some(NumericKind::Signed(_)) | Some(NumericKind::Float(_))
                );
                if !negatable {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::InvalidNegateOperand { r#type: checked_base.r#type },
                    ));
                    return None;
                }

                let r#type = checked_base.r#type.clone();
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type,
                    kind: CheckedExpr::Negate(Box::new(checked_base)),
                })
            }

            HirExpr::Increment(base) => self.analyze_incr_decr(node_id, span, base, BinaryOp::Add),
            HirExpr::Decrement(base) => self.analyze_incr_decr(node_id, span, base, BinaryOp::Sub),

            HirExpr::BinaryOp(bin) => {
                let checked_left = self.analyze_expr(&bin.left)?;
                let checked_right = self.analyze_expr(&bin.right)?;

                for operand in [&checked_left, &checked_right] {
                    if operand.r#type.numeric_kind().is_none() {
                        self.errors.push(AnalysisError::new(
                            node_id,
                            span,
                            AnalysisErrorKind::InvalidBinaryOperand {
                                op: bin.op,
                                r#type: operand.r#type.clone(),
                            },
                        ));
                        return None;
                    }
                }

                // No implicit numeric conversions anywhere else in this
                // language (see e.g. `AssignmentTypeMismatch`) -- arithmetic
                // between two different numeric types is no exception.
                if checked_left.r#type != checked_right.r#type {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::BinaryOperandTypeMismatch {
                            left: checked_left.r#type.clone(),
                            right: checked_right.r#type.clone(),
                        },
                    ));
                    return None;
                }

                // No native float remainder instruction (see
                // `AnalysisErrorKind::FloatRemainder`'s doc comment) --
                // matching C, which requires `fmod`/`fmodf` instead of `%`.
                if bin.op == BinaryOp::Rem
                    && matches!(checked_left.r#type.numeric_kind(), Some(NumericKind::Float(_)))
                {
                    self.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::FloatRemainder));
                    return None;
                }

                // A comparison always produces `bool`, regardless of the
                // (still-numeric, still-matching) operand type; an
                // arithmetic op's result is that same operand type.
                let r#type = if bin.op.is_comparison() { ResolvedType::Bool } else { checked_left.r#type.clone() };
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type,
                    kind: CheckedExpr::BinaryOp(CheckedBinaryOp {
                        op: bin.op,
                        left: Box::new(checked_left),
                        right: Box::new(checked_right),
                    }),
                })
            }

            HirExpr::ArrayLiteral(elements) => {
                let Some((first, rest)) = elements.split_first() else {
                    self.errors
                        .push(AnalysisError::new(node_id, span, AnalysisErrorKind::EmptyArrayLiteral));
                    return None;
                };

                let checked_first = self.analyze_expr(first)?;
                let item_type = checked_first.r#type.clone();
                let mut checked_elements = Vec::with_capacity(elements.len());
                checked_elements.push(checked_first);

                for element in rest {
                    let checked_element = self.analyze_expr(element)?;
                    if checked_element.r#type != item_type {
                        self.errors.push(AnalysisError::new(
                            element.id,
                            element.span,
                            AnalysisErrorKind::ArrayElementTypeMismatch {
                                expected: item_type.clone(),
                                found: checked_element.r#type.clone(),
                            },
                        ));
                        return None;
                    }
                    checked_elements.push(checked_element);
                }

                let size = checked_elements.len() as u32;
                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::SizedArray(Box::new(item_type.clone()), size),
                    kind: CheckedExpr::ArrayLiteral(CheckedArrayLiteral { item_type, elements: checked_elements }),
                })
            }

            HirExpr::Slice(HirSlice { base, start, end }) => {
                let (checked_base, base_type) = self.analyze_place(node_id, span, base)?;

                let item_type = match base_type {
                    ResolvedType::SizedArray(item_type, _) => *item_type,
                    ResolvedType::Slice(item_type) => *item_type,
                    _ => {
                        self.errors
                            .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotSliceable));
                        return None;
                    }
                };

                let analyze_bound = |this: &mut Self, bound: &Option<Box<HirExprNode>>| -> Option<Option<Box<CheckedExprNode>>> {
                    let Some(bound) = bound else { return Some(None) };
                    let checked_bound = this.analyze_expr(bound)?;
                    if checked_bound.r#type != ResolvedType::I32 {
                        this.errors.push(AnalysisError::new(
                            bound.id,
                            bound.span,
                            AnalysisErrorKind::InvalidSliceBound { r#type: checked_bound.r#type },
                        ));
                        return None;
                    }
                    Some(Some(Box::new(checked_bound)))
                };

                let checked_start = analyze_bound(self, start)?;
                let checked_end = analyze_bound(self, end)?;

                Some(CheckedExprNode {
                    id: node_id,
                    span,
                    r#type: ResolvedType::Slice(Box::new(item_type.clone())),
                    kind: CheckedExpr::Slice(CheckedSlice {
                        base: checked_base,
                        item_type,
                        start: checked_start,
                        end: checked_end,
                    }),
                })
            }
        }
    }

    /// Desugars `ident := value;` into the same two `CheckedStmt`s writing
    /// `ident : <inferred type>; ident = value;` by hand would produce --
    /// analysis is the only place that can do this desugaring, since only it
    /// knows `value`'s resolved type (there's nothing written down to carry
    /// a type otherwise). `value` is analyzed exactly once and reused as the
    /// assignment's value, rather than re-analyzed, to avoid double-reporting
    /// any error inside it.
    fn analyze_walrus(&mut self, w: &HirWalrusDeclaration) -> Option<[CheckedStmt; 2]> {
        let checked_value = self.analyze_expr(&w.value)?;
        let r#type = checked_value.r#type.clone();
        self.declare_binding(w.id, w.span, &w.ident, r#type.clone(), Storage::Local)?;

        let declaration = CheckedStmt::Declaration(CheckedDeclaration {
            id: w.id,
            span: w.span,
            ident: w.ident.clone(),
            r#type: r#type.clone(),
        });
        let assignment = CheckedStmt::Expression(CheckedExprNode {
            id: w.id,
            span: w.span,
            r#type: r#type.clone(),
            kind: CheckedExpr::Assignment(CheckedAssignment {
                target: CheckedPlace {
                    root: CheckedPlaceRoot::Variable { decl_id: w.id, storage: Storage::Local, r#type },
                    projections: vec![],
                },
                value: Box::new(checked_value),
            }),
        });

        Some([declaration, assignment])
    }

    /// Most statements analyze into exactly one `CheckedStmt`; a walrus
    /// declaration desugars into two (see `analyze_walrus`), which is why
    /// this returns a `Vec` rather than routing through the 1-to-1
    /// `analyze_all` fold.
    fn analyze_stmt(&mut self, stmt: &HirStmt) -> Option<Vec<CheckedStmt>> {
        match stmt {
            HirStmt::Declaration(decl) => {
                self.analyze_declaration(decl, Storage::Local).map(|d| vec![CheckedStmt::Declaration(d)])
            }
            HirStmt::ExternDeclaration(decl) => {
                self.analyze_extern_decl(decl).map(|d| vec![CheckedStmt::ExternDeclaration(d)])
            }
            HirStmt::Expression(expr) => self.analyze_expr(expr).map(|e| vec![CheckedStmt::Expression(e)]),
            HirStmt::Return(expr) => {
                let checked = self.analyze_expr(expr)?;
                if checked.r#type != self.current_return_type {
                    self.errors.push(AnalysisError::new(
                        expr.id,
                        expr.span,
                        AnalysisErrorKind::ReturnTypeMismatch {
                            expected: self.current_return_type.clone(),
                            found: checked.r#type.clone(),
                        },
                    ));
                    return None;
                }
                Some(vec![CheckedStmt::Return(checked)])
            }
            HirStmt::Struct(struct_def) => self.analyze_struct_def(struct_def).map(|s| vec![CheckedStmt::Struct(s)]),
            HirStmt::WalrusDeclaration(w) => self.analyze_walrus(w).map(Vec::from),
            HirStmt::While(w) => {
                let checked_cond = self.analyze_expr(&w.condition)?;
                if checked_cond.r#type != ResolvedType::Bool {
                    self.errors.push(AnalysisError::new(
                        w.id,
                        w.span,
                        AnalysisErrorKind::NonBoolCondition { r#type: checked_cond.r#type },
                    ));
                    return None;
                }
                self.loop_stack.push(w.id);
                let checked_body = self.analyze_block(&w.body);
                self.loop_stack.pop();
                let checked_body = checked_body?;
                Some(vec![CheckedStmt::While(CheckedWhile {
                    id: w.id,
                    span: w.span,
                    condition: checked_cond,
                    body: checked_body,
                })])
            }
            HirStmt::For(f) => self.analyze_for(f),
            HirStmt::Break(b) => match self.loop_stack.last() {
                Some(&loop_id) => Some(vec![CheckedStmt::Break(CheckedBreak { id: b.id, span: b.span, loop_id })]),
                None => {
                    self.errors.push(AnalysisError::new(b.id, b.span, AnalysisErrorKind::BreakOutsideLoop));
                    None
                }
            },
            HirStmt::Continue(c) => match self.loop_stack.last() {
                Some(&loop_id) => {
                    Some(vec![CheckedStmt::Continue(CheckedContinue { id: c.id, span: c.span, loop_id })])
                }
                None => {
                    self.errors.push(AnalysisError::new(c.id, c.span, AnalysisErrorKind::ContinueOutsideLoop));
                    None
                }
            },
        }
    }

    /// `for`'s init/condition/post/body all share one scope of their own
    /// (so an `i := 0` init clause is visible to the condition/post/body
    /// but doesn't leak past the loop) -- entered once here, around all
    /// four, rather than the body getting its own additional nested scope
    /// from `analyze_block` alone. Every sub-part is still analyzed even
    /// after an earlier one fails (same "keep going, report everything"
    /// discipline as `analyze_all`), and the scope is always left before
    /// any early return.
    fn analyze_for(&mut self, f: &HirFor) -> Option<Vec<CheckedStmt>> {
        self.context.enter_scope();

        let mut ok = true;

        let checked_init = self.analyze_stmts(&f.init);
        ok &= checked_init.is_some();

        let checked_condition = match &f.condition {
            Some(c) => match self.analyze_expr(c) {
                Some(cc) if cc.r#type != ResolvedType::Bool => {
                    self.errors.push(AnalysisError::new(
                        f.id,
                        f.span,
                        AnalysisErrorKind::NonBoolCondition { r#type: cc.r#type },
                    ));
                    ok = false;
                    None
                }
                Some(cc) => Some(cc),
                None => {
                    ok = false;
                    None
                }
            },
            None => {
                self.errors
                    .push(AnalysisError::new(f.id, f.span, AnalysisErrorKind::ForLoopMissingCondition));
                ok = false;
                None
            }
        };

        let checked_post = match &f.post {
            Some(p) => {
                let checked = self.analyze_expr(p);
                ok &= checked.is_some();
                checked
            }
            None => None,
        };

        self.loop_stack.push(f.id);
        let checked_body = self.analyze_block(&f.body);
        self.loop_stack.pop();
        ok &= checked_body.is_some();

        self.context.leave_scope();

        if !ok {
            return None;
        }

        Some(vec![CheckedStmt::For(Box::new(CheckedFor {
            id: f.id,
            span: f.span,
            init: checked_init?,
            condition: checked_condition?,
            post: checked_post,
            body: checked_body?,
        }))])
    }

    fn analyze_stmts(&mut self, stmts: &[HirStmt]) -> Option<Vec<CheckedStmt>> {
        let mut checked = Vec::with_capacity(stmts.len());
        let mut ok = true;
        for stmt in stmts {
            match self.analyze_stmt(stmt) {
                Some(mut items) => checked.append(&mut items),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        Some(self.truncate_unreachable(checked))
    }

    /// A function's declared return type must match its body's effective
    /// type (see `block_type`) -- a tail expression of the right type, or an
    /// unconditional trailing `return` (already individually type-checked
    /// against `current_return_type` when it was analyzed, so nothing more
    /// to check there), or (only for `Void`) falling off the end with no
    /// tail at all.
    fn check_function_return(
        &mut self,
        id: HirId,
        span: SimpleSpan,
        return_type: &ResolvedType,
        body: &CheckedBlock,
    ) -> Option<()> {
        match Self::block_type(body) {
            None => Some(()),
            Some(found) if found == *return_type => Some(()),
            Some(found) => {
                self.errors.push(AnalysisError::new(
                    id,
                    span,
                    AnalysisErrorKind::ReturnTypeMismatch { expected: return_type.clone(), found },
                ));
                None
            }
        }
    }

    fn analyze_function_def(&mut self, f: &HirFunctionDef) -> Option<CheckedFunctionDef> {
        self.context.enter_scope();
        let params = self.analyze_all(&f.params, Self::analyze_param);
        let return_type = self.resolve_type_or_error(f.id, f.span, &f.return_type);

        // Saved/restored (not just set) since a struct -- and therefore its
        // methods -- can be declared inside a function body, nesting one
        // function's analysis inside another's; see `current_return_type`'s
        // and `loop_stack`'s doc comments. A method's body starts with no
        // enclosing loop of its own, regardless of whether the `struct`
        // declaring it sits inside one.
        let previous_return_type = std::mem::replace(
            &mut self.current_return_type,
            return_type.clone().unwrap_or(ResolvedType::Void),
        );
        let previous_loop_stack = std::mem::take(&mut self.loop_stack);
        let body = self.analyze_block(&f.body);
        self.current_return_type = previous_return_type;
        self.loop_stack = previous_loop_stack;

        self.context.leave_scope();

        let params = params?;
        let return_type = return_type?;
        let body = body?;
        self.check_function_return(f.id, f.span, &return_type, &body)?;

        let checked = CheckedFunctionDef {
            id: f.id,
            span: f.span,
            name: f.name.clone(),
            is_member_function: f.is_member_function,
            is_variadic: false,
            params,
            return_type,
            body,
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
            // Processed separately, before either pass ever reaches
            // `analyze_item`/`collect_item_signature` -- see
            // `process_imports`.
            HirItem::Import(_) => {
                unreachable!("imports are processed by process_imports, before analyze_item runs")
            }
        }
    }

    /// `import a::b::c;` -- resolves what the path actually names (a whole
    /// module, or an item inside one; see `ModuleResolver::resolve_import`'s
    /// doc comment for why that's not decidable from syntax alone) and binds
    /// the result under the path's last segment, exactly like
    /// `Context::new` already seeds builtin primitives: one mechanism for
    /// "a name is available in this scope," reused.
    fn process_import(&mut self, import: &HirImport) -> Option<()> {
        let alias = import.path.tail.last().cloned().unwrap_or_else(|| import.path.head.clone());
        match self.resolver.resolve_import(&import.path) {
            Ok(ImportTarget::Module(absolute)) => {
                self.context.import_module(alias, absolute);
                Some(())
            }
            Ok(ImportTarget::Item(resolved)) => match self.context.bind_imported_item(alias, resolved) {
                Ok(()) => Some(()),
                Err(dup) => {
                    self.errors
                        .push(AnalysisError::new(import.id, import.span, AnalysisErrorKind::Redeclaration(dup)));
                    None
                }
            },
            Err(e) => {
                self.errors
                    .push(AnalysisError::new(import.id, import.span, AnalysisErrorKind::ModuleResolution(e)));
                None
            }
        }
    }

    /// Every `import` in `items`, processed before anything else -- so a
    /// module's own items can freely use any of them regardless of where in
    /// the file they were textually written (imports are root-level-only
    /// syntax, so there's no meaningful "before its import" case the way a
    /// same-module forward function reference has -- see the "explicitly out
    /// of scope" note on `collect_signatures`).
    fn process_imports(&mut self, items: &[HirItem]) -> bool {
        let mut ok = true;
        for item in items {
            if let HirItem::Import(import) = item {
                ok &= self.process_import(import).is_some();
            }
        }
        ok
    }

    /// A function's *signature* only: param and return types, with no scope
    /// entered and no param bound by name -- binding is a body-analysis-time
    /// concern (nothing needs to call a param by name yet), so this is
    /// strictly less work than `analyze_function_def`, not a restricted
    /// version of it. Registers the function's own name in the current scope
    /// exactly like `analyze_function_def` does, so later same-module items
    /// (and this module's own exported `ModuleSignature`) can see it.
    fn collect_function_signature(&mut self, f: &HirFunctionDef) -> Option<ResolvedFunctionType> {
        let params = self.analyze_all(&f.params, |this, p| {
            this.resolve_type_or_error(p.id, p.span, &p.r#type).map(|t| (p.ident.clone(), t))
        })?;
        let return_type = self.resolve_type_or_error(f.id, f.span, &f.return_type)?;
        let fn_type = ResolvedFunctionType {
            params,
            return_type: Box::new(return_type),
            is_variadic: false,
            is_member_function: f.is_member_function,
        };

        let binding = VarBinding {
            decl_id: f.id,
            storage: Storage::Function,
            r#type: ResolvedType::Function(fn_type.clone()),
        };
        if let Err(dup) = self.context.current_scope().declare(f.name.clone(), binding) {
            self.errors
                .push(AnalysisError::new(f.id, f.span, AnalysisErrorKind::Redeclaration(dup)));
            return None;
        }

        Some(fn_type)
    }

    /// A struct's *signature* only: field types (via `analyze_struct_fields`,
    /// already signature-only -- struct fields are never scope-bound names,
    /// so it has no body-shaped work to skip in the first place) plus every
    /// method's signature, with zero recursion into any method body. Mirrors
    /// `analyze_struct_def`'s early-self-insertion trick unchanged (a
    /// method's synthetic `self: *StructName` parameter needs the struct's
    /// own name to already resolve).
    fn collect_struct_signature(&mut self, s: &HirStructDef) -> Option<ResolvedType> {
        let fields = self.analyze_struct_fields(&s.fields)?;

        self.context.current_scope().defined_types.insert(
            s.name.clone(),
            ResolvedType::Struct(ResolvedStructType {
                fields: fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect(),
                functions: vec![],
            }),
        );

        self.context.enter_scope();
        let functions = self.analyze_all(&s.functions, Self::collect_function_signature);
        self.context.leave_scope();
        let functions = functions?;

        let ResolvedType::Struct(resolved) = self
            .context
            .current_scope()
            .defined_types
            .get_mut(&s.name)
            .expect("just inserted above, in this exact scope frame")
        else {
            unreachable!("just inserted as ResolvedType::Struct above");
        };
        resolved.functions = s
            .functions
            .iter()
            .zip(functions)
            .map(|(f, fn_type)| (f.name.clone(), ResolvedMethod { decl_id: f.id, fn_type }))
            .collect();

        Some(self.context.find_defined_type(&s.name).expect("just inserted above").clone())
    }

    /// Collects one module's exported signature table: every top-level
    /// item's *signature* (no bodies) plus, for a struct, every method's
    /// signature -- everything another module's qualified path could ever
    /// need. Processes items in the same top-to-bottom order
    /// `analyze_bodies` does, deliberately *not* fixed to tolerate
    /// same-module forward references or self-recursive-by-value struct
    /// cycles: doing that soundly needs turning same-module resolution into
    /// a genuine on-demand recursive query too (the same trap a naive
    /// insert-placeholders-then-patch scheme falls into -- see the plan's
    /// "explicitly out of scope" section), which is a materially bigger,
    /// separately-risky change than this feature needs. Cross-module cycles
    /// (a *different* module's signature transitively requiring this one's)
    /// are instead caught by the driver, at module granularity, before this
    /// is ever called recursively for the same module twice.
    pub fn collect_signatures(&mut self, hir_module: &HirModule) -> Result<ModuleSignature, Vec<AnalysisError>> {
        let mut ok = self.process_imports(&hir_module.items);
        let mut signature = ModuleSignature::default();

        for item in &hir_module.items {
            let entry = match item {
                HirItem::Import(_) => None,
                HirItem::Declaration(decl) => self.analyze_declaration(decl, Storage::Global).map(|checked| {
                    (checked.ident.clone(), ResolvedItem::Value {
                        r#type: checked.r#type,
                        storage: Storage::Global,
                        decl_id: checked.id,
                    })
                }),
                HirItem::ExternDeclaration(decl) => self.analyze_extern_decl(decl).map(|checked| {
                    let storage = if matches!(checked.r#type, ResolvedType::Function(_)) {
                        Storage::Function
                    } else {
                        Storage::Global
                    };
                    (checked.ident.clone(), ResolvedItem::Value { r#type: checked.r#type, storage, decl_id: checked.id })
                }),
                HirItem::FunctionDefinition(f) => self.collect_function_signature(f).map(|fn_type| {
                    (f.name.clone(), ResolvedItem::Value {
                        r#type: ResolvedType::Function(fn_type),
                        storage: Storage::Function,
                        decl_id: f.id,
                    })
                }),
                HirItem::Struct(s) => {
                    self.collect_struct_signature(s).map(|r#type| (s.name.clone(), ResolvedItem::Type(r#type)))
                }
            };

            match entry {
                Some((name, item)) => {
                    signature.items.insert(name, SignatureEntry { visibility: Visibility::Public, item });
                }
                None if matches!(item, HirItem::Import(_)) => {}
                None => ok = false,
            }
        }

        if ok { Ok(signature) } else { Err(std::mem::take(&mut self.errors)) }
    }

    /// Analyzes every item's *body* -- almost exactly what this used to be
    /// (`Analyzer::analyze`) before signature collection split out into its
    /// own pass, with one behavioral addition: qualified-path type/place
    /// resolution can now reach across modules via `resolver` instead of
    /// always failing locally. Independent of `collect_signatures` for the
    /// *same* module -- it reprocesses this module's own imports and
    /// top-level items itself (cheap; any cross-module lookup hits the
    /// driver's cache) rather than reusing that pass's output, so no
    /// half-built state has to survive between the two passes.
    pub fn analyze_bodies(
        mut self,
        hir_module: &HirModule,
    ) -> Result<(CheckedModule, Vec<AnalysisWarning>), Vec<AnalysisError>> {
        let mut ok = self.process_imports(&hir_module.items);

        let mut items = Vec::new();
        for item in hir_module.items.iter().filter(|item| !matches!(item, HirItem::Import(_))) {
            match self.analyze_item(item) {
                Some(checked) => items.push(checked),
                None => ok = false,
            }
        }

        if ok && self.errors.is_empty() {
            Ok((CheckedModule { id: hir_module.id, items }, self.warnings))
        } else {
            Err(self.errors)
        }
    }
}

