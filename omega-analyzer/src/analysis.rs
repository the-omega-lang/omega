use crate::{
    checked::{
        CheckedAddressOf, CheckedArrayLiteral, CheckedAssignment, CheckedBinaryOp, CheckedBlock,
        CheckedBreak, CheckedContinue, CheckedDeclaration, CheckedDefer, CheckedExpr,
        CheckedExprNode, CheckedExternDeclaration, CheckedFor, CheckedFunctionCall, CheckedFunctionDef,
        CheckedIf,
        CheckedParam, CheckedPlace, CheckedPlaceRoot,
        CheckedProjection, CheckedSlice, CheckedStmt, CheckedStructDef, CheckedWhile, NumberValue,
        Storage,
    },
    context::{Context, VarBinding},
    error::{AnalysisError, AnalysisErrorKind, AnalysisWarning, AnalysisWarningKind},
    generics::unify_generic_type,
    resolved_type::{NumericKind, ResolvedFunctionType, ResolvedMethod, ResolvedStructType, ResolvedType},
    resolver::{GenericSignature, ImportTarget, ModuleResolver, ResolvedImport, ResolvedItem},
};
use omega_hir::{
    BinaryOp, HirAddressOf, HirBlock, HirDeclaration, HirExpr, HirExprNode, HirExternDeclaration,
    HirFor, HirFunctionCall, HirFunctionDef, HirId, HirIf, HirItem, HirParam, HirPlace, HirPlaceRoot,
    HirProjection, HirSlice, HirStmt, HirStructDef, HirWalrusDeclaration,
};
use omega_parser::prelude::{Ident, NumberBase, NumberExpr, Span, Type};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

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
    /// This item's owning module's absolute path -- supplies the implicit
    /// prefix an unqualified top-level reference needs to become an
    /// absolute `(module_path, name)` query, so it's resolved exactly the
    /// same way a qualified cross-module reference is (see
    /// `ModuleResolver::resolve_item`'s doc comment: there is no longer an
    /// architectural difference between the two). The *same* path for
    /// every item constructed for this module, whether it's this module's
    /// own top-level signature/body work or -- unchanged from before --
    /// this same `module_path` threading through a locally-nested
    /// `HirStmt::Struct`'s own field resolution too.
    module_path: Vec<Ident>,
    /// Names currently mid-resolution *within this one throwaway
    /// `Analyzer`* -- today, only ever populated by `analyze_struct_def`
    /// around a *locally-nested* (block-scoped) struct's own field
    /// resolution, to detect that one struct including itself by value
    /// (`RecursiveTypeWithoutIndirection`). Top-level items no longer use
    /// this: that bookkeeping is global now, owned by
    /// `omega_driver::Driver::query_state` (keyed by `(module_path, name)`,
    /// not just `name`), since a same-module reference and a cross-module
    /// one are resolved by the exact same mechanism.
    in_progress: HashSet<Ident>,
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
    /// `true` while analyzing a `defer`'s own body (see `HirStmt::Defer`'s
    /// arm in `analyze_stmt`) -- not a stack/counter, since a `defer` nested
    /// inside another defer's body is rejected outright the moment this is
    /// already `true`, so it can never need to represent more than one
    /// level. Used to reject `return` inside a defer body (it would have to
    /// jump into the very epilogue that runs deferred bodies, from inside
    /// one of them) and nested `defer`. Saved and restored around each
    /// `check_function_body` call, exactly like `current_return_type`/
    /// `loop_stack`: a struct declared *inside* a defer's body still gets
    /// methods with entirely ordinary bodies of their own, not ones that
    /// inherit "we're inside a defer" from their lexical surroundings.
    in_defer_body: bool,
}

/// A top-level item's own name, or `None` for an `import` (which binds no
/// name of its own -- see `Context::import_module`/`bind_imported_item`
/// instead). Exposed for `omega_driver::Driver`, which now owns the
/// per-module "every named top-level item" index (`local_items`) that used
/// to live on `Analyzer` -- one item is resolved (and one `Analyzer`
/// constructed) at a time now, so there's no module-wide sweep left inside
/// this crate to share this with locally.
pub fn item_name(item: &HirItem) -> Option<Ident> {
    match item {
        HirItem::Declaration(d) => Some(d.ident.clone()),
        HirItem::ExternDeclaration(d) => Some(d.ident.clone()),
        HirItem::FunctionDefinition(f) => Some(f.name.clone()),
        HirItem::Struct(s) => Some(s.name.clone()),
        HirItem::Import(_) => None,
    }
}

/// A top-level item's own `HirId`/`Span`, for anchoring a
/// `Redeclaration` error against a duplicate name -- see `item_name`.
pub fn item_id_span(item: &HirItem) -> (HirId, Span) {
    match item {
        HirItem::Declaration(d) => (d.id, d.span),
        HirItem::ExternDeclaration(d) => (d.id, d.span),
        HirItem::FunctionDefinition(f) => (f.id, f.span),
        HirItem::Struct(s) => (s.id, s.span),
        HirItem::Import(i) => (i.id, i.span),
    }
}

impl<'r> Analyzer<'r> {
    /// `imports` is this module's already-resolved import aliases/items --
    /// computed once per module and cached by `omega_driver::Driver`
    /// (cycle-guarded there too, since resolving one module's item-style
    /// imports can itself need another module's), then applied fresh here
    /// into every throwaway `Analyzer` built for one of this module's
    /// items. This replaces the old per-call `process_import`/
    /// `process_imports` -- imports are processed exactly once per module
    /// now (by whoever builds this list), not once per item.
    ///
    /// `generics` is the concrete substitution for the item's own declared
    /// generic parameters -- empty for an ordinary, non-generic item.
    /// Seeded into `defined_types` *after* imports (through the same
    /// duplicate-checking discipline: colliding with an already-bound import
    /// alias, a builtin, or another entry in `generics` itself is a
    /// `Redeclaration`, anchored at `owner` -- the item's own id/span, since
    /// an individual generic parameter has none of its own). This is what
    /// makes a generic parameter nothing more than a type name bound to a
    /// concrete `ResolvedType` for the lifetime of one throwaway `Analyzer`:
    /// genericity is purely a resolution-time concern, matching the
    /// "duck typed" design (no bounds are ever declared or checked here).
    pub fn new(
        resolver: &'r mut dyn ModuleResolver,
        module_path: Vec<Ident>,
        imports: &[ResolvedImport],
        generics: &[(Ident, ResolvedType)],
        owner: (HirId, Span),
    ) -> Self {
        let mut context = Context::new();
        let mut errors = Vec::new();
        for import in imports {
            let result = match import.target.clone() {
                ImportTarget::Module(absolute) => {
                    context.import_module(import.alias.clone(), absolute);
                    Ok(())
                }
                ImportTarget::Item(resolved) => context.bind_imported_item(import.alias.clone(), resolved),
                ImportTarget::GenericItem(absolute) => {
                    context.bind_generic_alias(import.alias.clone(), absolute);
                    Ok(())
                }
            };
            if let Err(dup) = result {
                errors.push(AnalysisError::new(import.id, import.span, AnalysisErrorKind::Redeclaration(dup)));
            }
        }

        let mut seen_generics = HashSet::new();
        for (ident, resolved_type) in generics {
            let dup = context.current_scope().defined_types.contains_key(ident) || !seen_generics.insert(ident);
            if dup {
                errors.push(AnalysisError::new(owner.0, owner.1, AnalysisErrorKind::Redeclaration(ident.clone())));
            } else {
                context.current_scope().defined_types.insert(ident.clone(), resolved_type.clone());
            }
        }

        Self {
            errors,
            warnings: vec![],
            context,
            resolver,
            module_path,
            in_progress: HashSet::new(),
            current_return_type: ResolvedType::Void,
            loop_stack: vec![],
            in_defer_body: false,
        }
    }

    /// Consumes this throwaway, per-item `Analyzer`, handing back whatever
    /// it accumulated -- `omega_driver::Driver` folds these into its own
    /// per-module `module_errors`/warnings after every signature/body call.
    pub fn finish(self) -> (Vec<AnalysisError>, Vec<AnalysisWarning>) {
        (self.errors, self.warnings)
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

    /// `indirect` is true whenever `typ` sits somewhere that never embeds
    /// its referent inline into another type's layout -- a function's own
    /// param/return types, or anything already behind a `Pointer`/`Array`/
    /// `Slice` -- as opposed to a struct field or `SizedArray` element,
    /// which do. See `ModuleResolver::resolve_item`'s doc comment for why
    /// this distinction is what separates a legitimate self-reference
    /// (`next: *Node`) from a genuine infinite-size cycle (`value: Node`).
    /// The on-demand triggering that used to happen in a separate pre-pass
    /// here now happens inline, inside `Context::resolve_type` itself (it
    /// calls the resolver directly on an unqualified miss), so this is just
    /// a thin error-reporting wrapper around it.
    fn resolve_type_or_error(&mut self, id: HirId, span: Span, typ: &Type, indirect: bool) -> Option<ResolvedType> {
        match self.context.resolve_type(typ.to_owned(), &mut *self.resolver, &self.module_path, indirect) {
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
        span: Span,
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

    pub fn analyze_declaration(&mut self, decl: &HirDeclaration, storage: Storage) -> Option<CheckedDeclaration> {
        // A global's type is never itself embedded inline into another
        // type's layout (it isn't a struct field), so it can never be part
        // of an infinite-size cycle -- always indirect.
        let resolved_type = self.resolve_type_or_error(decl.id, decl.span, &decl.r#type, true)?;
        self.declare_binding(decl.id, decl.span, &decl.ident, resolved_type.clone(), storage)?;
        Some(CheckedDeclaration {
            id: decl.id,
            span: decl.span,
            ident: decl.ident.clone(),
            r#type: resolved_type,
        })
    }

    pub fn analyze_extern_decl(&mut self, extern_decl: &HirExternDeclaration) -> Option<CheckedExternDeclaration> {
        let resolved_type = self.resolve_type_or_error(extern_decl.id, extern_decl.span, &extern_decl.r#type, true)?;
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
        Some(CheckedExternDeclaration {
            id: extern_decl.id,
            span: extern_decl.span,
            ident: extern_decl.ident.clone(),
            r#type: resolved_type,
        })
    }

    fn analyze_param(&mut self, param: &HirParam) -> Option<CheckedParam> {
        // A parameter is passed by value at the call site, not laid out
        // inline inside anything -- a method taking its own struct type by
        // value (`fn combine(self, other: Self) -> Self`) is completely
        // ordinary and must not be flagged as a layout cycle.
        let resolved_type = self.resolve_type_or_error(param.id, param.span, &param.r#type, true)?;
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
            // A field is the one context that genuinely lays its type out
            // inline -- this is the case `RecursiveTypeWithoutIndirection`
            // exists to catch, so it's the only caller passing `false`.
            let resolved_type = this.resolve_type_or_error(field.id, field.span, &field.r#type, false)?;
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
        span: Span,
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
        let struct_type = struct_type.borrow();

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
        let struct_type = struct_type.borrow();
        if struct_type.fields.iter().any(|(name, _)| name == field) {
            return None;
        }
        struct_type
            .functions
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, method)| method.clone())
    }

    /// Resolves `absolute` (already a full `[module_path.., name]`, whether
    /// built from a qualified place's import alias or an unqualified one's
    /// implicit own-module prefix) to a place root -- shared by both of
    /// `analyze_place`'s non-local cases so the `Value`/`Type`/`Err` match
    /// is only written once.
    fn resolve_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: Vec<Ident>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        match self.resolver.resolve_item(&absolute, &[], true) {
            Ok(ResolvedItem::Value { r#type, storage, decl_id }) => {
                let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                Some((root, r#type))
            }
            Ok(ResolvedItem::Type(_)) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::NotAValue(absolute)));
                None
            }
            Err(e) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                None
            }
        }
    }

    /// Resolves a place's root, then folds over its projections in source
    /// order, resolving field/index/deref projections against the running
    /// type and recording the exact resolved shape (field index, item/
    /// pointee type) so codegen never has to re-search or re-derive them.
    fn analyze_place(
        &mut self,
        node_id: HirId,
        span: Span,
        place: &HirPlace,
    ) -> Option<(CheckedPlace, ResolvedType)> {
        let (root, mut current_type) = match &place.root {
            // An unqualified place root -- a local (function-body-level)
            // binding wins if there is one; otherwise this is a same-module
            // top-level reference, resolved the exact same way a qualified
            // cross-module one is, with `module_path` supplying the
            // implicit prefix (see `resolve_qualified_value`). Values never
            // need the indirect/in-progress distinction type resolution
            // does -- only a named *type* can ever be legitimately
            // mid-collection when referenced (see
            // `ModuleResolver::resolve_item`'s doc comment) -- so this
            // always passes `true`.
            HirPlaceRoot::Path(path) if path.is_unqualified() => {
                let ident = &path.head;
                if let Some(binding) = self.context.find_variable(ident) {
                    let root = CheckedPlaceRoot::Variable {
                        decl_id: binding.decl_id,
                        storage: binding.storage,
                        r#type: binding.r#type.clone(),
                    };
                    (root, binding.r#type.clone())
                } else {
                    // A generic-item import alias (see `Context::
                    // generic_alias`) takes priority over the implicit
                    // own-module prefix, exactly like `Context::
                    // resolve_absolute_item_path` does for types -- this
                    // only ever reaches here for a *non-call* reference to a
                    // generic function (a call goes through
                    // `resolve_generic_call` first), which has no way to
                    // supply type arguments; `ensure_item` reports that
                    // uniformly as `GenericArgCountMismatch` rather than
                    // this falling through to (and possibly silently
                    // matching) an unrelated same-named item in this module.
                    let absolute = match self.context.generic_alias(ident) {
                        Some(absolute) => absolute.clone(),
                        None => self.module_path.iter().cloned().chain(std::iter::once(ident.clone())).collect(),
                    };
                    self.resolve_qualified_value(node_id, span, absolute)?
                }
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
                self.resolve_qualified_value(node_id, span, absolute)?
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

    /// If `call`'s callee is a bare (optionally module-qualified) reference
    /// to a *generic* function, resolves the whole call here via duck-typed,
    /// argument-driven type inference instead of the ordinary
    /// `resolve_callee`-then-args pipeline, and returns `Some(result)`
    /// (`result` itself `None` on a reported error, `Some` on success) --
    /// the caller must not fall through to the ordinary path either way, to
    /// avoid re-analyzing/double-reporting. Returns plain `None` (untouched)
    /// for anything that isn't this shape, so the caller proceeds with the
    /// ordinary path exactly as if this were never called:
    ///
    /// - a method-call shape (`base.method(...)`, i.e. the callee's last
    ///   projection is a `FieldAccess`) -- struct generics are always
    ///   explicit (`List<u32>`), so by the time a value of that type exists,
    ///   its methods are already fully monomorphized; no special call-site
    ///   handling is needed there at all;
    /// - a callee that isn't a bare/qualified path with zero projections;
    /// - a path shadowed by a local (function-body-level) binding -- always
    ///   wins, and is never generic (only top-level items can be);
    /// - a qualified path whose head isn't a recognized import alias -- left
    ///   for the ordinary path to report `UndefinedVariable`;
    /// - `generic_function_signature` reporting this isn't a generic
    ///   function (including "doesn't exist," or a generic *struct* --
    ///   neither is this call's concern).
    fn resolve_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
    ) -> Option<Option<CheckedExprNode>> {
        let HirExpr::Place(place) = &call.callee.expr else { return None };
        if !place.projections.is_empty() {
            return None;
        }
        let HirPlaceRoot::Path(path) = &place.root else { return None };

        if path.is_unqualified() && self.context.find_variable(&path.head).is_some() {
            return None;
        }

        let absolute = if path.is_unqualified() {
            match self.context.generic_alias(&path.head) {
                Some(absolute) => absolute.clone(),
                None => self.module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect(),
            }
        } else {
            self.context.absolute_path(path)?
        };

        let sig: GenericSignature = match self.resolver.generic_function_signature(&absolute) {
            Ok(Some(sig)) => sig,
            Ok(None) => return None,
            Err(_) => return None,
        };

        Some(self.finish_generic_call(node_id, span, call, &absolute, &sig))
    }

    /// The actual work behind `resolve_generic_call`, once it's confirmed
    /// `call`'s callee genuinely names a generic function at `absolute` --
    /// split out so `resolve_generic_call` can stay a single `?`-chained
    /// "does this even apply" check.
    fn finish_generic_call(
        &mut self,
        node_id: HirId,
        span: Span,
        call: &HirFunctionCall,
        absolute: &[Ident],
        sig: &GenericSignature,
    ) -> Option<CheckedExprNode> {
        let mut checked_args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            checked_args.push(self.analyze_expr(arg)?);
        }

        let mut subst = HashMap::new();
        for (raw_type, arg) in sig.params.iter().zip(&checked_args) {
            unify_generic_type(&sig.generics, raw_type, &arg.r#type, &mut subst);
        }

        let mut type_args = Vec::with_capacity(sig.generics.len());
        for generic in &sig.generics {
            match subst.get(generic) {
                Some(resolved) => type_args.push(resolved.clone()),
                None => {
                    self.errors.push(AnalysisError::new(
                        node_id,
                        span,
                        AnalysisErrorKind::UnresolvedGenericParam(generic.clone()),
                    ));
                    return None;
                }
            }
        }

        let (fn_type, storage, decl_id) = match self.resolver.resolve_item(absolute, &type_args, true) {
            Ok(ResolvedItem::Value { r#type: ResolvedType::Function(fn_type), storage, decl_id }) => {
                (fn_type, storage, decl_id)
            }
            Ok(_) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::UnresolvedCallee));
                return None;
            }
            Err(e) => {
                self.errors
                    .push(AnalysisError::new(node_id, span, AnalysisErrorKind::ModuleResolution(e)));
                return None;
            }
        };

        if checked_args.len() != fn_type.params.len() && !fn_type.is_variadic {
            self.errors.push(AnalysisError::new(
                node_id,
                span,
                AnalysisErrorKind::TooManyArguments { expected: fn_type.params.len() },
            ));
            return None;
        }
        for (arg, (_, expected_type)) in checked_args.iter().zip(&fn_type.params) {
            if arg.r#type != *expected_type {
                self.errors.push(AnalysisError::new(
                    arg.id,
                    arg.span,
                    AnalysisErrorKind::ArgumentTypeMismatch {
                        expected: expected_type.clone(),
                        found: arg.r#type.clone(),
                    },
                ));
                return None;
            }
        }

        let callee_node = CheckedExprNode {
            id: call.callee.id,
            span: call.callee.span,
            r#type: ResolvedType::Function(fn_type.clone()),
            kind: CheckedExpr::Place(CheckedPlace {
                root: CheckedPlaceRoot::Variable { decl_id, storage, r#type: ResolvedType::Function(fn_type.clone()) },
                projections: vec![],
            }),
        };

        Some(CheckedExprNode {
            id: node_id,
            span,
            r#type: (*fn_type.return_type).clone(),
            kind: CheckedExpr::FunctionCall(CheckedFunctionCall { callee: Box::new(callee_node), fn_type, args: checked_args }),
        })
    }

    /// Resolves a number literal's target type (explicit suffix, or the
    /// default -- `f64` for a literal with a decimal point, `i32` otherwise,
    /// mirroring Rust's own literal defaults) and parses/range-checks its
    /// text against that type. `NumberExpr` keeps its digits as plain text
    /// (see its doc comment) precisely so this is the *only* place that ever
    /// has to interpret them -- codegen just emits whatever `NumberValue`
    /// this produces.
    fn analyze_number(&mut self, node_id: HirId, span: Span, n: &NumberExpr) -> Option<CheckedExprNode> {
        let invalid_suffix = |this: &mut Self, ident: &Ident| {
            this.errors.push(AnalysisError::new(node_id, span, AnalysisErrorKind::InvalidNumberType(ident.clone())));
        };

        let resolved_type = match &n.explicit_type {
            Some(explicit_type) => match self.context.resolve_type(
                Type::Named(explicit_type.clone().into()),
                &mut *self.resolver,
                &self.module_path,
                true,
            ) {
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
            // `defer` never diverges at its own position -- it just marks
            // "reached" and moves on; the deferred body only ever runs later,
            // in the function's epilogue.
            CheckedStmt::Defer(_) => false,
            _ => false,
        }
    }

    /// Every `CheckedStmt` variant's id/span, for anchoring an
    /// `AnalysisWarningKind::UnreachableCode` at whichever statement turns
    /// out to be first made unreachable by a diverging predecessor (see
    /// `truncate_unreachable`).
    fn checked_stmt_id_span(stmt: &CheckedStmt) -> (HirId, Span) {
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
            CheckedStmt::Defer(d) => (d.id, d.span),
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
    fn analyze_incr_decr(&mut self, node_id: HirId, span: Span, base: &HirExprNode, op: BinaryOp) -> Option<CheckedExprNode> {
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
                if let Some(result) = self.resolve_generic_call(node_id, span, call) {
                    return result;
                }

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
                if self.in_defer_body {
                    self.errors.push(AnalysisError::new(expr.id, expr.span, AnalysisErrorKind::ReturnInsideDefer));
                    return None;
                }
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
            HirStmt::Defer(d) => {
                if !self.loop_stack.is_empty() {
                    self.errors.push(AnalysisError::new(d.id, d.span, AnalysisErrorKind::DeferInsideLoopNotSupported));
                    return None;
                }
                if self.in_defer_body {
                    self.errors.push(AnalysisError::new(d.id, d.span, AnalysisErrorKind::NestedDeferNotSupported));
                    return None;
                }
                let previous_in_defer_body = std::mem::replace(&mut self.in_defer_body, true);
                let body = self.analyze_block(&d.body);
                self.in_defer_body = previous_in_defer_body;
                let body = body?;
                Some(vec![CheckedStmt::Defer(CheckedDefer { id: d.id, span: d.span, body })])
            }
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
        span: Span,
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

    /// A *locally-nested* (block-scoped, `HirStmt::Struct`) struct's method:
    /// top-level functions/methods never reach this anymore (see
    /// `check_function_body`) -- this is just `collect_function_signature`
    /// (resolve the signature, bind the name) immediately followed by
    /// checking the body against it, which is what actually fixes
    /// self-recursion here: the function's own name is bound *before* its
    /// body is checked, not after, so a call to itself inside that body
    /// resolves instead of hitting `UndefinedVariable`.
    fn analyze_function_def(&mut self, f: &HirFunctionDef) -> Option<CheckedFunctionDef> {
        if !f.generics.is_empty() {
            self.errors
                .push(AnalysisError::new(f.id, f.span, AnalysisErrorKind::NestedGenericsNotSupported));
            return None;
        }
        let fn_type = self.collect_function_signature(f)?;
        self.check_function_body(f, &fn_type, f.id)
    }

    /// A *locally-nested* (block-scoped, `HirStmt::Struct`) struct
    /// definition -- top-level structs never reach this anymore (see
    /// `check_struct_body`). Same placeholder-before-fields shape as
    /// `collect_struct_signature`, just without any of `local_items`'s
    /// cross-item bookkeeping: there are no textual siblings to forward-
    /// reference here, only (potentially) this one struct's own name, so
    /// `in_progress` is pushed/popped directly around field resolution
    /// rather than through `ensure_item_signature`.
    fn analyze_struct_def(&mut self, s: &HirStructDef) -> Option<CheckedStructDef> {
        if !s.generics.is_empty() {
            self.errors
                .push(AnalysisError::new(s.id, s.span, AnalysisErrorKind::NestedGenericsNotSupported));
            return None;
        }
        let cell = Rc::new(RefCell::new(ResolvedStructType {
            id: s.id,
            name: s.name.clone(),
            fields: vec![],
            functions: vec![],
        }));
        // TODO: Make sure type does not already exist
        self.context
            .current_scope()
            .defined_types
            .insert(s.name.clone(), ResolvedType::Struct(cell.clone()));

        self.in_progress.insert(s.name.clone());
        let fields = self.analyze_struct_fields(&s.fields);
        self.in_progress.remove(&s.name);
        let fields = fields?;
        cell.borrow_mut().fields = fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect();

        // Methods are bound in their own nested scope so they aren't
        // globally callable; `resolve_type` still sees the struct's type
        // just inserted above by walking outward through the scope stack.
        self.context.enter_scope();
        let functions = self.analyze_all(&s.functions, Self::analyze_function_def);
        self.context.leave_scope();
        let functions = functions?;
        cell.borrow_mut().functions = functions
            .iter()
            .map(|f| (f.name.clone(), ResolvedMethod { decl_id: f.id, fn_type: f.fn_type() }))
            .collect();

        Some(CheckedStructDef { id: s.id, span: s.span, name: s.name.clone(), fields, functions })
    }

    /// A function's *signature* only: param and return types, with no scope
    /// entered and no param bound by name -- binding is a body-analysis-time
    /// concern (nothing needs to call a param by name yet), so this is
    /// strictly less work than `check_function_body`, not a restricted
    /// version of it. Registers the function's own name in the current
    /// (throwaway) scope too -- inert for a top-level function (nothing else
    /// ever looks at this particular `Context` again; `omega_driver::Driver`
    /// reads the *return value*, not this binding), but this same method
    /// also runs once per sibling method inside `signature_of_struct`'s
    /// method loop, where it *does* matter: it's what catches two methods
    /// sharing a name on one struct.
    pub fn collect_function_signature(&mut self, f: &HirFunctionDef) -> Option<ResolvedFunctionType> {
        // Param/return types are a function's signature, never inline data --
        // always indirect (see `analyze_param`'s identical reasoning).
        let params = self.analyze_all(&f.params, |this, p| {
            this.resolve_type_or_error(p.id, p.span, &p.r#type, true).map(|t| (p.ident.clone(), t))
        })?;
        let return_type = self.resolve_type_or_error(f.id, f.span, &f.return_type, true)?;
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

    /// A top-level struct's *signature* only: field types, plus every
    /// method's signature, with zero recursion into any method body. Unlike
    /// the pre-cross-module-cycle-fix version of this, `cell` is created (and
    /// registered in `omega_driver::Driver`'s global `struct_cells`, keyed by
    /// `(module_path, name)`) by the *caller* before this ever runs, not by
    /// this method itself -- so a self-referencing field (`next: *Node`) or a
    /// same- or cross-module mutual one resolves via `Context::resolve_type`'s
    /// resolver fallback finding this exact struct already `InProgress` in
    /// `Driver`'s global query state, not via anything local to this one
    /// throwaway `Analyzer`/`Context`. This method's only job is to populate
    /// `cell` in place, patched via `RefCell` so every earlier clone of it
    /// (e.g. one taken for a pointer field while this was still empty)
    /// observes the final result too.
    /// `method_ids` supplies, positionally (one per `s.functions`), the
    /// `HirId` each method's `ResolvedMethod.decl_id` gets stamped with --
    /// `f.id` itself for an ordinary (non-generic) struct, or a freshly
    /// minted synthetic id per generic instantiation (decided once by
    /// `omega_driver::Driver::compute_item`, the single source of truth for
    /// instantiation identity -- see its doc comment). `check_struct_body`
    /// reads these same ids back out of `cell` rather than ever recomputing
    /// them, so both phases agree on one identity per instantiation.
    pub fn signature_of_struct(
        &mut self,
        s: &HirStructDef,
        cell: &Rc<RefCell<ResolvedStructType>>,
        method_ids: &[HirId],
    ) -> Option<()> {
        let fields = self.analyze_struct_fields(&s.fields)?;
        cell.borrow_mut().fields = fields.iter().map(|f| (f.ident.clone(), f.r#type.clone())).collect();

        self.context.enter_scope();
        let functions = self.analyze_all(&s.functions, Self::collect_function_signature);
        self.context.leave_scope();
        let functions = functions?;
        cell.borrow_mut().functions = s
            .functions
            .iter()
            .zip(functions)
            .zip(method_ids)
            .map(|((f, fn_type), &decl_id)| (f.name.clone(), ResolvedMethod { decl_id, fn_type }))
            .collect();

        Some(())
    }

    /// Checks a function's (or method's) *body* only -- its signature, and
    /// its own name bound so any call to it (including a recursive one from
    /// its own body) resolves, are already handled by
    /// `omega_driver::Driver::ensure_item`/`collect_function_signature`.
    /// Enters a fresh scope to bind `f`'s params by name (signature
    /// collection only ever resolved their *types*, never bound them --
    /// that's a body-analysis-time concern, same as it always was).
    /// `id` is stamped onto the produced `CheckedFunctionDef` in place of
    /// always reading `f.id` -- for an ordinary (non-generic) function this
    /// is just `f.id` (behavior-preserving); for a generic instantiation
    /// it's the same freshly-minted synthetic id `omega_driver::Driver`
    /// already decided (and stored) during the signature phase, so codegen
    /// gets one distinct compiled function per instantiation.
    pub fn check_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
    ) -> Option<CheckedFunctionDef> {
        self.context.enter_scope();
        let params = self.analyze_all(&f.params, Self::analyze_param);

        // Saved/restored (not just set) since a struct -- and therefore its
        // methods -- can be declared inside a function body, nesting one
        // function's analysis inside another's; see `current_return_type`'s
        // and `loop_stack`'s doc comments. A method's body starts with no
        // enclosing loop of its own, regardless of whether the `struct`
        // declaring it sits inside one.
        let previous_return_type =
            std::mem::replace(&mut self.current_return_type, (*fn_type.return_type).clone());
        let previous_loop_stack = std::mem::take(&mut self.loop_stack);
        let previous_in_defer_body = std::mem::replace(&mut self.in_defer_body, false);
        let body = self.analyze_block(&f.body);
        self.current_return_type = previous_return_type;
        self.loop_stack = previous_loop_stack;
        self.in_defer_body = previous_in_defer_body;

        self.context.leave_scope();

        let params = params?;
        let body = body?;
        self.check_function_return(f.id, f.span, &fn_type.return_type, &body)?;

        Some(CheckedFunctionDef {
            id,
            span: f.span,
            name: f.name.clone(),
            is_member_function: f.is_member_function,
            is_variadic: false,
            params,
            return_type: (*fn_type.return_type).clone(),
            body,
        })
    }

    /// Checks a top-level struct's methods' *bodies* only -- its fields and
    /// every method's signature are already fully resolved (see
    /// `signature_of_struct`), sitting in `cell` (the same shared cell
    /// `omega_driver::Driver` created before that ran); `fields`/`functions`
    /// here are read back out of it (zipped positionally against
    /// `s.fields`/`s.functions`, in the same order `signature_of_struct`
    /// built them in) rather than recomputed, so a self-referencing field
    /// reads back the exact same live data `analyze_place`'s field
    /// projections will later see.
    pub fn check_struct_body(&mut self, s: &HirStructDef, cell: &Rc<RefCell<ResolvedStructType>>) -> Option<CheckedStructDef> {
        let fields = s
            .fields
            .iter()
            .zip(cell.borrow().fields.iter())
            .map(|(hir_field, (_, r#type))| CheckedParam {
                id: hir_field.id,
                span: hir_field.span,
                ident: hir_field.ident.clone(),
                r#type: r#type.clone(),
            })
            .collect();

        // `decl_id` is read back from the cell (not `f.id`) so a generic
        // instantiation's methods get the same synthetic ids the signature
        // phase already decided -- see `signature_of_struct`'s doc comment.
        let methods: Vec<(ResolvedFunctionType, HirId)> =
            cell.borrow().functions.iter().map(|(_, method)| (method.fn_type.clone(), method.decl_id)).collect();

        self.context.enter_scope();
        let mut functions = Vec::with_capacity(s.functions.len());
        let mut ok = true;
        for (f, (fn_type, decl_id)) in s.functions.iter().zip(methods.iter()) {
            match self.check_function_body(f, fn_type, *decl_id) {
                Some(checked) => functions.push(checked),
                None => ok = false,
            }
        }
        self.context.leave_scope();
        if !ok {
            return None;
        }

        Some(CheckedStructDef { id: s.id, span: s.span, name: s.name.clone(), fields, functions })
    }
}

