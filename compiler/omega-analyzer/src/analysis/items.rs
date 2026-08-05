use super::*;

/// An enum's resolved shared header -- the tag type (explicit or the
/// implicit `u16`), whether the tag was declared explicitly, and the
/// per-variant constant fields following it.
struct EnumHeader {
    tag_type: ResolvedType,
    has_tag: bool,
    fields: Vec<(Ident, ResolvedType, Visibility)>,
}

impl EnumHeader {
    /// Whether `name` is already taken by the tag or a header field -- the
    /// names no dynamic or body field may reuse.
    fn claims(&self, name: &Ident) -> bool {
        name.as_ref() == "tag" || self.fields.iter().any(|(field, _, _)| field == name)
    }
}

impl<'r> Analyzer<'r> {
    pub fn analyze_declaration(&mut self, decl: &HirDeclaration, storage: Storage) -> Option<CheckedDeclaration> {
        // A global's type is never itself embedded inline into another
        // type's layout (it isn't a struct field), so it can never be part
        // of an infinite-size cycle -- always indirect.
        let resolved_type = self.resolve_type_or_error(decl.id, decl.span, &decl.r#type, true)?;
        self.declare_binding(decl.id, decl.span, &decl.ident, resolved_type.clone(), storage, decl.mutable)?;
        Some(CheckedDeclaration {
            id: decl.id,
            span: decl.span,
            ident: decl.ident.clone(),
            r#type: resolved_type,
            mutable: decl.mutable,
            // `analyze_declaration` never receives an initializer -- a
            // local with one goes through `DeclarationWithInit` instead,
            // and a top-level binding with one is a `Walrus`, handled by
            // `analyze_global_walrus` below, which builds its own
            // `CheckedDeclaration` directly rather than through here.
            initial_value: None,
        })
    }

    /// `comp ident := value;` at item level (`HirItem::Walrus`, `w.comp ==
    /// true` -- the driver only ever calls this once it's already decided
    /// that, see `Driver::compute_item`'s `HirItem::Walrus` arm) -- unlike
    /// a local `comp` binding (`Analyzer::analyze_walrus`'s own `comp`
    /// branch), this never calls `declare_binding`/`Context::
    /// set_comp_value`: a top-level binding's identity and value must
    /// survive past this one throwaway `Analyzer`, so the driver
    /// (`Driver::compute_item`) records them in its own cross-item state
    /// (`ItemQueries::comp_values`) instead, once this returns. `w.value`
    /// needs no explicit inner `comp` of its own -- the binding's own
    /// `comp` is already the one unambiguous marker this whole line means
    /// "evaluate at compile time" (unlike `analyze_global_walrus` below,
    /// where that explicitness is exactly what's needed to tell a
    /// genuinely-runtime initializer apart from a compile-time-known one).
    pub fn analyze_comp_declaration(&mut self, w: &HirWalrusDeclaration) -> Option<(ResolvedType, ConstValue)> {
        if w.mutable {
            self.error(w.id, w.span, AnalysisErrorKind::MutCompBinding);
            return None;
        }
        let checked = self.analyze_expr(&w.value, None)?;
        let r#type = checked.r#type.clone();
        let value = self.eval_comp(w.id, &checked)?;
        Some((r#type, value))
    }

    /// `ident := value;` at item level, without `comp` on the binding
    /// (`HirItem::Walrus`, `w.comp == false`) -- a real `Storage::Global`
    /// place, not substituted, but still requiring a compile-time-known
    /// initial value: no runtime constructor/init-order machinery exists
    /// (see `docs/19-compile-time-evaluation.md`), so `value`'s only
    /// legal shape here is one that's *already* `CheckedExpr::Const` by
    /// the time ordinary analysis finishes with it -- exactly what an
    /// explicit `comp <expr>` (or any of the handful of already-const-
    /// recognized literal positions `analysis/consts.rs` handles) already
    /// collapses into. This is a check on the analyzed *result*'s shape,
    /// not a re-derivation of "was the word `comp` written somewhere" --
    /// strictly more general, and it's the same signal every other
    /// compile-time-known-or-not distinction in this compiler already
    /// uses.
    pub fn analyze_global_walrus(&mut self, w: &HirWalrusDeclaration) -> Option<CheckedDeclaration> {
        let checked = self.analyze_expr(&w.value, None)?;
        self.finish_global_binding(w.id, w.span, &w.ident, w.mutable, &w.value, checked)
    }

    /// `ident : Type = value;` at item level (`HirItem::DeclarationWithInit`)
    /// -- `analyze_global_walrus`'s explicitly-typed sibling, and the
    /// top-level counterpart of `analyze_declaration_with_init` (locals):
    /// `value` is analyzed *with* the declared type as an `expected` hint
    /// (so an unsuffixed literal picks it, e.g. `abc : u64 = 10;`),
    /// coerced, and checked for acceptance exactly like the local version
    /// -- the only thing added here is `finish_global_binding`'s shared
    /// "must already be compile-time-known" requirement, which a plain
    /// local declaration never needed (a local's value becomes an ordinary
    /// runtime `Assignment`, not baked-in static data).
    pub fn analyze_global_declaration_with_init(
        &mut self,
        decl: &HirDeclaration,
        value: &HirExprNode,
    ) -> Option<CheckedDeclaration> {
        let resolved_type = self.resolve_type_or_error(decl.id, decl.span, &decl.r#type, true)?;
        let checked_value = self.analyze_expr(value, Some(&resolved_type))?;
        let checked_value = self.coerce_to_expected(Some(&resolved_type), checked_value);
        if !resolved_type.accepts(&checked_value.r#type) {
            self.error(
                value.id,
                value.span,
                AnalysisErrorKind::AssignmentTypeMismatch {
                    target: resolved_type,
                    value: checked_value.r#type,
                },
            );
            return None;
        }
        self.finish_global_binding(decl.id, decl.span, &decl.ident, decl.mutable, value, checked_value)
    }

    /// The shared tail `analyze_global_walrus` and
    /// `analyze_global_declaration_with_init` both need once `value` is
    /// fully analyzed (and, for the typed form, already coerced/accepted
    /// against its declared type): enforce the one rule every non-`comp`
    /// top-level binding shares (`value` must be compile-time-known),
    /// then register and build the `CheckedDeclaration` both shapes
    /// produce identically from there. `raw_value` (the pre-analysis HIR
    /// node) is only needed for `recognize_top_level_literal`'s fallback
    /// below -- `checked_value` alone can't tell "a bare `10`" apart from
    /// something else that happened to analyze to the same type.
    fn finish_global_binding(
        &mut self,
        id: HirId,
        span: Span,
        ident: &Ident,
        mutable: bool,
        raw_value: &HirExprNode,
        checked_value: CheckedExprNode,
    ) -> Option<CheckedDeclaration> {
        let r#type = checked_value.r#type.clone();
        let const_value = match checked_value.kind {
            CheckedExpr::Const(v) => v,
            // Not already `comp`-evaluated -- still worth checking whether
            // it's a plain literal (`10`, `"hi"`, `&[1, 2]`, ...) before
            // giving up: a literal never needed evaluating in the first
            // place, so it shouldn't need `comp` either. Reports its own
            // error on failure (`?` propagates it), so nothing further is
            // reported here.
            _ => self.recognize_top_level_literal(raw_value, &r#type)?,
        };
        self.declare_binding(id, span, ident, r#type.clone(), Storage::Global, mutable)?;
        Some(CheckedDeclaration { id, span, ident: ident.clone(), r#type, mutable, initial_value: Some(const_value) })
    }

    /// Whether `expr`'s *raw* HIR shape is a literal this compiler already
    /// recognizes as inherently compile-time-known with no `comp` needed
    /// anywhere -- `analysis/consts.rs`'s `const_eval`/`const_eval_slice`
    /// boundary (a number/string/bool/char, or an array/slice literal
    /// built from more of the same, recursively), reused here as a
    /// fallback once `finish_global_binding`'s caller has already
    /// established `expected` as this expression's own, already-validated
    /// type.
    ///
    /// Deliberately **not** the same function as `const_eval`/
    /// `const_eval_slice`, and not just for their usual "differently-
    /// worded errors" reason (see `const_eval_slice`'s own doc comment):
    /// those two *also* fall back to the general `comp` interpreter for
    /// any shape they don't otherwise recognize, since an enum header or
    /// a `&[...]` position is already unambiguously compile-time-only.
    /// A top-level binding is not that -- `10 + 20` or a function call is
    /// genuine computation, which still needs an explicit `comp` here, so
    /// this has no such fallback: an unrecognized shape is just rejected.
    ///
    /// Reports exactly one error on every failure path (either a specific
    /// one via `const_number`, or `TopLevelValueNotComp` for anything
    /// else), so `finish_global_binding` can propagate `None` via `?`
    /// without risking a second, redundant diagnostic on top.
    fn recognize_top_level_literal(&mut self, expr: &HirExprNode, expected: &ResolvedType) -> Option<ConstValue> {
        let not_comp = |this: &mut Self| {
            this.error(expr.id, expr.span, AnalysisErrorKind::TopLevelValueNotComp);
            None
        };
        match &expr.expr {
            HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, false).map(ConstValue::Number),
            HirExpr::Negate(inner) => match &inner.expr {
                HirExpr::Number(n) => self.const_number(expr.id, expr.span, n, expected, true).map(ConstValue::Number),
                _ => not_comp(self),
            },
            HirExpr::String(s) => match expected {
                ResolvedType::Str { mutable: false } => Some(ConstValue::Str(s.0.clone())),
                _ => not_comp(self),
            },
            HirExpr::Bool(b) => match expected {
                ResolvedType::Bool => Some(ConstValue::Bool(*b)),
                _ => not_comp(self),
            },
            HirExpr::Char(c) => match expected {
                ResolvedType::Char => Some(ConstValue::Char(*c)),
                _ => not_comp(self),
            },
            HirExpr::ArrayLiteral(elements) => match expected {
                ResolvedType::SizedArray(item, size) if elements.len() == *size as usize => {
                    let mut values = Vec::with_capacity(elements.len());
                    for element in elements {
                        values.push(self.recognize_top_level_literal(element, item)?);
                    }
                    Some(ConstValue::Array(values))
                }
                _ => not_comp(self),
            },
            // `&[...]` is the only recognized spelling for a compile-time
            // slice, matching `const_eval`/`const_eval_slice`'s identical
            // rule -- a bare `[...]` is never treated as one, and `&mut
            // [...]` isn't recognized here at all (falls through to
            // `not_comp`, same as any other unrecognized shape).
            HirExpr::AddressOf(HirAddressOf { base, mutable: false }) => match &base.expr {
                HirExpr::ArrayLiteral(elements) => match expected {
                    ResolvedType::Slice { item, mutable: false } => {
                        let mut values = Vec::with_capacity(elements.len());
                        for element in elements {
                            values.push(self.recognize_top_level_literal(element, item)?);
                        }
                        Some(ConstValue::Slice(values))
                    }
                    _ => not_comp(self),
                },
                _ => not_comp(self),
            },
            _ => not_comp(self),
        }
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
        // `extern` declarations are always immutable for now -- no existing
        // use case needs mutable extern data, and `mut extern` can be added
        // later without breaking anything (see `omega_parser`'s `mut`
        // contextual-keyword sites, none of which check for it here).
        self.declare_binding(
            extern_decl.id,
            extern_decl.span,
            &extern_decl.ident,
            resolved_type.clone(),
            storage,
            false,
        )?;
        Some(CheckedExternDeclaration {
            id: extern_decl.id,
            span: extern_decl.span,
            ident: extern_decl.ident.clone(),
            r#type: resolved_type,
            mangling: crate::annotations::ManglingMode::Disabled,
        })
    }

    fn analyze_param(&mut self, param: &HirParam) -> Option<CheckedParam> {
        // A parameter is passed by value at the call site, not laid out
        // inline inside anything -- a method taking its own struct type by
        // value (`fn combine(self, other: Self) -> Self`) is completely
        // ordinary and must not be flagged as a layout cycle.
        let resolved_type = self.resolve_type_or_error(param.id, param.span, &param.r#type, true)?;
        // Parameters (including `self`) are always immutable bindings --
        // `mut` is never recognized in parameter position at all (see
        // `omega_parser::parser::item::parse_declaration_list`); a
        // parameter that needs to vary locally can be shadowed
        // (`mut x := param;`). `self`'s own *pointee* mutability (`mut
        // self` vs `self`) is a separate, `ResolvedType::Pointer` concern,
        // already baked into `resolved_type` here.
        self.declare_binding(param.id, param.span, &param.ident, resolved_type.clone(), Storage::Parameter, false)?;
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
        let mut seen: HashMap<Ident, Span> = HashMap::new();
        self.analyze_all(fields, |this, field| {
            if let Some(previous) = seen.insert(field.ident.clone(), field.span) {
                this.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::Redeclaration { name: field.ident.clone(), previous: Some(previous) },
                );
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
            Some(found) if return_type.accepts(&found) => Some(()),
            Some(found) => {
                self.error(id, span, AnalysisErrorKind::ReturnTypeMismatch { expected: return_type.clone(), found });
                None
            }
        }
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
    /// Note this deliberately does *not* declare `f.name` into
    /// `self.context`'s current scope the way most other signature
    /// collection does -- when this runs once per sibling inside
    /// `signature_of_struct`/`enum`/`union`'s method loop, that binding
    /// would never actually be visible to anything (body-checking runs
    /// later, through an entirely separate `Analyzer`/`Context`; see
    /// `omega_driver::Driver::check_item_body`), so its *only* real effect
    /// was catching two methods sharing a name -- which up to two
    /// *overloaded* methods are now allowed to do (see
    /// `check_overload_duplicates`, called by each of those three methods
    /// once every sibling's signature is known). A top-level (non-method)
    /// caller never had a meaningful use for the binding either -- it
    /// always got a fresh, empty `Context`, so nothing could ever collide.
    /// A function's (or method's) signature *and* its resolved
    /// `@inline`/`@mangling`/`@suppress` -- resolved together, here, once,
    /// and never again at body-check time (see `check_function_body`'s own
    /// doc comment for why): this is the one point in the whole pipeline
    /// that's guaranteed to run for *every* function this compilation knows
    /// about, whether its body is ever actually checked here or not (an
    /// extern-owned function/method, referenced via `--extern`, only ever
    /// has its signature collected -- see
    /// `omega_driver::Driver::collect_extern_functions`). Resolving
    /// annotations anywhere body-only would silently strand that
    /// information from anything that only ever sees the signature, which
    /// is exactly the bug `@mangling(disabled)` on an extern function used
    /// to have.
    /// `return_type_override` is `Some` only for a `spec T` (static-
    /// dispatch) return-type function, whose concrete return type has
    /// already been discovered by `infer_body_return_type` before this ever
    /// runs (see `omega_driver::Driver::resolve_spec_return_function`) --
    /// `f.return_type` itself is `Type::SpecStatic` in that case, which
    /// ordinary `resolve_type_or_error` has no concrete answer for (ordinary
    /// type resolution rejects it outright -- see
    /// `TypeResolutionError::SpecStaticNotAllowedHere`). `None` for every
    /// other function, resolving `f.return_type` exactly as before.
    pub fn collect_function_signature(
        &mut self,
        f: &HirFunctionDef,
        return_type_override: Option<ResolvedType>,
    ) -> Option<(ResolvedFunctionType, crate::annotations::ResolvedAnnotations)> {
        // Param/return types are a function's signature, never inline data --
        // always indirect (see `analyze_param`'s identical reasoning).
        let params = self.analyze_all(&f.params, |this, p| {
            this.resolve_type_or_error(p.id, p.span, &p.r#type, true).map(|t| (p.ident.clone(), t))
        })?;

        for (p, (_, r#type)) in f.params.iter().zip(params.iter()) {
            if matches!(r#type, ResolvedType::Struct(_) | ResolvedType::Union(_) | ResolvedType::Enum { .. }) {
                let size = crate::annotations::estimate_type_size(r#type);
                if size > crate::annotations::LARGE_STRUCT_BY_VALUE_THRESHOLD {
                    self.warn(p.id, p.span, AnalysisWarningKind::LargeStructByValue { r#type: r#type.clone(), size });
                }
            }
        }

        let return_type = match return_type_override {
            Some(r#type) => r#type,
            None => self.resolve_return_type_or_error(f.id, f.span, &f.return_type, true)?,
        };
        let annotations = crate::annotations::resolve(
            self,
            f.id,
            &f.annotations,
            crate::annotations::ItemKind::Function,
            f.self_mode.is_some(),
            !f.generics.is_empty(),
        );
        Some((
            ResolvedFunctionType {
                params,
                return_type: Box::new(return_type),
                is_variadic: false,
                self_mode: f.self_mode,
            },
            annotations,
        ))
    }

    /// Compares every pair of `functions`' signatures by param-type list,
    /// ignoring parameter names -- the method-loop counterpart to
    /// `omega_driver::Driver::check_overload_duplicates` (see its doc
    /// comment for the full reasoning): two methods sharing a name is a
    /// valid overload as long as their signatures genuinely differ; an
    /// identical pair is a real duplicate, reported the same way a plain
    /// same-name collision always has been.
    ///
    /// A second, self-aware test runs alongside the first: when both
    /// candidates are member functions and their *non-self* parameters
    /// match, that's ambiguous too, even though the full parameter lists
    /// (self included) differ -- a call site (`obj.method(...)`) has no
    /// syntax to pick "receives self by value" vs. "by pointer", so self's
    /// own mode can never be the sole thing distinguishing two overloads
    /// (see `AnalysisErrorKind::AmbiguousSelfOverload`).
    fn check_overload_duplicates(
        &mut self,
        functions: &[HirFunctionDef],
        signatures: &[(ResolvedFunctionType, crate::annotations::ResolvedAnnotations)],
    ) {
        for i in 1..functions.len() {
            for j in 0..i {
                if functions[i].name != functions[j].name {
                    continue;
                }
                let (sig_i, sig_j) = (&signatures[i].0, &signatures[j].0);
                let same_params = sig_i.params.iter().map(|(_, t)| t).eq(sig_j.params.iter().map(|(_, t)| t));
                if same_params {
                    self.error(
                        functions[i].id,
                        functions[i].span,
                        AnalysisErrorKind::Redeclaration {
                            name: functions[i].name.clone(),
                            previous: Some(functions[j].span),
                        },
                    );
                    break;
                }
                if sig_i.self_mode.is_some() && sig_j.self_mode.is_some() {
                    let same_rest = sig_i.params[1..].iter().map(|(_, t)| t).eq(sig_j.params[1..].iter().map(|(_, t)| t));
                    if same_rest {
                        self.error(
                            functions[i].id,
                            functions[i].span,
                            AnalysisErrorKind::AmbiguousSelfOverload {
                                name: functions[i].name.clone(),
                                previous: functions[j].span,
                            },
                        );
                        break;
                    }
                }
            }
        }
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
    /// One item's own `@...` annotations, resolved once here at signature
    /// time so everything downstream (including a consumer that only ever
    /// sees the signature, never the body) reads back the same values.
    fn item_annotations(
        &mut self,
        id: HirId,
        annotations: &[omega_hir::HirAnnotation],
        kind: crate::annotations::ItemKind,
    ) -> crate::annotations::ResolvedAnnotations {
        // The two `false`s are `is_member_function`/`is_generic`, which only
        // gate `@mangling(disabled)` on a *function*.
        crate::annotations::resolve(self, id, annotations, kind, false, false)
    }

    /// The tail every aggregate's signature shares: resolve each declared
    /// method's own signature, reject two that no call could tell apart,
    /// then fold in whatever the `implements` clause additionally requires.
    /// `method_ids` are the identities the driver already decided for this
    /// instantiation (see `ModuleResolver::fresh_synthetic_id`).
    ///
    /// Returns the full method list to store on the cell, plus every
    /// spec-default body still owed a phase-2 check.
    fn collect_methods(
        &mut self,
        owner: (HirId, Span),
        name: &Ident,
        functions: &[omega_hir::HirFunctionDef],
        implements: &[Type],
        method_ids: &[HirId],
        self_type: &ResolvedType,
        glue: bool,
    ) -> Option<SpecMethods> {
        self.context.enter_scope();
        // A struct/enum/union method never yet supports `spec T` return-type
        // body inference (`return_type_override: None`, unconditionally) --
        // that machinery only exists for a free function so far (see
        // `omega_driver::Driver::resolve_spec_return_function`, which is
        // only ever triggered from `compute_item`'s own top-level
        // `HirFunctionDefinition` arm); a method whose return type is bare
        // `spec T` gets the same `SpecStaticNotAllowedHere` rejection any
        // other unsupported position does.
        let signatures = self.analyze_all(functions, |this, f| this.collect_function_signature(f, None));
        self.context.leave_scope();
        let signatures = signatures?;
        self.check_overload_duplicates(functions, &signatures);

        let mut own: Vec<(Ident, ResolvedMethod)> = functions
            .iter()
            .zip(signatures)
            .zip(method_ids)
            .map(|((f, (fn_type, annotations)), &decl_id)| {
                (f.name.clone(), ResolvedMethod { decl_id, fn_type, visibility: f.visibility, annotations })
            })
            .collect();

        let (from_specs, pending, implemented_specs) =
            self.resolve_implements_clause(owner.0, owner.1, name, implements, &own, self_type, glue);

        // `@glue`'s actual wiring: for every gap this marker implements,
        // any of its *own* methods matching one of that gap's own function
        // names (by name -- the signature match was already verified by
        // ordinary spec conformance, above) gets its mangled symbol forced
        // to match the gap's own expected one (`ManglingMode::Glued`). Only
        // `own` is ever eligible -- a method inherited from a spec default
        // is compiled once, in the gap's own declaring module, never
        // per-glue (see `docs/21-gaps-and-glue.md`).
        if glue {
            for (spec, _) in &implemented_specs {
                let spec = spec.borrow();
                if !spec.is_gap {
                    continue;
                }
                for (fn_name, _) in &spec.functions {
                    if let Some((_, method)) = own.iter_mut().find(|(name, _)| name == fn_name) {
                        method.annotations.mangling = crate::annotations::ManglingMode::Glued {
                            spec_module_path: spec.module_path.clone(),
                            spec_name: spec.name.clone(),
                            function_name: fn_name.clone(),
                        };
                    }
                }
            }
        }

        let mut all = own;
        all.extend(from_specs);
        Some((all, pending, implemented_specs))
    }

    /// A struct's fields and methods. `None` means this struct's signature
    /// failed; its own diagnostics were already recorded.
    pub fn signature_of_struct(
        &mut self,
        s: &HirStructDef,
        cell: &Rc<RefCell<ResolvedStructType>>,
        method_ids: &[HirId],
    ) -> Option<Vec<PendingSpecMethod>> {
        let annotations = self.item_annotations(s.id, &s.annotations, crate::annotations::ItemKind::Struct);
        cell.borrow_mut().layout = annotations.layout;
        cell.borrow_mut().suppress = annotations.suppress;
        cell.borrow_mut().is_marker = s.is_marker;
        cell.borrow_mut().is_glue = annotations.glue;
        // `@glue`'s "must be a marker" restriction can't be expressed as an
        // ordinary `ItemKind` applicability check (`@glue` still applies to
        // `ItemKind::Struct` as a whole -- see `ItemKind::Spec`'s doc
        // comment for why) -- checked here instead, right where `is_marker`
        // is already known, the same way `ZeroSizedAggregate`'s own marker
        // exemption is a few lines down.
        if annotations.glue && !s.generics.is_empty() {
            // Same reasoning as `GapMustNotBeGeneric`: `ManglingMode::Glued`
            // forces every matching method onto one fixed symbol, computed
            // from the *gap's* identity alone -- every instantiation of a
            // generic glue marker would collide on that identical symbol.
            self.error(s.id, s.span, AnalysisErrorKind::GlueMustNotBeGeneric);
        }
        if annotations.glue && !s.is_marker {
            self.error(s.id, s.span, AnalysisErrorKind::GlueOnNonMarker);
        }

        cell.borrow_mut().fields = self.resolve_declared_fields(&s.fields)?;

        let self_type = ResolvedType::Struct(cell.clone());
        // A `marker` is exempt by design (see `ResolvedStructType::
        // is_marker`'s doc comment); an ordinary struct isn't -- checked
        // against the type's own full leaf list (`layout::is_zero_sized`),
        // not just `s.fields.is_empty()`, so this also catches a struct
        // whose only field is itself zero-sized, and a generic struct that
        // only becomes zero-sized for one particular instantiation (this
        // runs once per instantiation, same as everything else here).
        if !s.is_marker && crate::layout::is_zero_sized(&self_type) {
            self.error(s.id, s.span, AnalysisErrorKind::ZeroSizedAggregate { name: s.name.clone(), is_union: false });
        }

        let (functions, pending, implemented_specs) = self.collect_methods(
            (s.id, s.span),
            &s.name,
            &s.functions,
            &s.implements,
            method_ids,
            &self_type,
            annotations.glue,
        )?;
        cell.borrow_mut().functions = functions;
        cell.borrow_mut().implemented_specs = implemented_specs;
        Some(pending)
    }

    /// A union's fields and methods -- identical to a struct's apart from
    /// having no layout annotation of its own (field overlap is a codegen
    /// concern, not a declared one).
    pub fn signature_of_union(
        &mut self,
        u: &HirUnionDef,
        cell: &Rc<RefCell<ResolvedUnionType>>,
        method_ids: &[HirId],
    ) -> Option<Vec<PendingSpecMethod>> {
        let annotations = self.item_annotations(u.id, &u.annotations, crate::annotations::ItemKind::Union);
        cell.borrow_mut().suppress = annotations.suppress;

        cell.borrow_mut().fields = self.resolve_declared_fields(&u.fields)?;

        let self_type = ResolvedType::Union(cell.clone());
        // Unions have no `marker` exemption -- see `signature_of_struct`'s
        // identical check for why this uses the full leaf list rather than
        // `u.fields.is_empty()`.
        if crate::layout::is_zero_sized(&self_type) {
            self.error(u.id, u.span, AnalysisErrorKind::ZeroSizedAggregate { name: u.name.clone(), is_union: true });
        }

        let (functions, pending, implemented_specs) = self.collect_methods(
            (u.id, u.span),
            &u.name,
            &u.functions,
            &u.implements,
            method_ids,
            &self_type,
            false, // `@glue` only applies to markers -- see `ItemKind::Spec`'s doc comment.
        )?;
        cell.borrow_mut().functions = functions;
        cell.borrow_mut().implemented_specs = implemented_specs;
        Some(pending)
    }

    /// One aggregate's declared fields, in the `(name, type, visibility)`
    /// shape a cell stores.
    fn resolve_declared_fields(
        &mut self,
        fields: &[HirParam],
    ) -> Option<Vec<(Ident, ResolvedType, Visibility)>> {
        let checked = self.analyze_struct_fields(fields)?;
        Some(
            fields
                .iter()
                .zip(checked)
                .map(|(declared, checked)| (checked.ident, checked.r#type, declared.visibility))
                .collect(),
        )
    }

    /// An enum's shared header: the tag (explicit, or the implicit `u16`)
    /// plus the per-variant constant fields following it.
    ///
    /// Every variant supplies one constant value per entry here, positionally
    /// -- so a broken header makes every variant's expectations (argument
    /// count, tag-ness, field types) unknowable, which is why resolving it
    /// is all-or-nothing.
    fn resolve_enum_header(&mut self, e: &HirEnumDef) -> Option<EnumHeader> {
        let mut ok = true;
        let mut explicit_tag: Option<ResolvedType> = None;
        let mut fields: Vec<(Ident, ResolvedType, Visibility)> = Vec::new();
        let mut seen: HashMap<Ident, Span> = HashMap::new();

        for (position, field) in e.header.iter().enumerate() {
            if field.ident.as_ref() == "tag" {
                match self.resolve_tag_type(field, position) {
                    Some(tag_type) => explicit_tag = Some(tag_type),
                    None => ok = false,
                }
                continue;
            }
            if seen.insert(field.ident.clone(), field.span).is_some() {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision { field: field.ident.clone(), variant: None },
                );
                ok = false;
                continue;
            }
            // Header fields are laid out inline in every enum value -- the
            // same `indirect = false` a struct field passes.
            let Some(resolved) = self.resolve_type_or_error(field.id, field.span, &field.r#type, false) else {
                ok = false;
                continue;
            };
            if !Self::const_representable(&resolved) {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumHeaderFieldUnsupportedType {
                        field: field.ident.clone(),
                        found: resolved,
                    },
                );
                ok = false;
                continue;
            }
            fields.push((field.ident.clone(), resolved, field.visibility));
        }

        ok.then(|| EnumHeader {
            has_tag: explicit_tag.is_some(),
            tag_type: explicit_tag.unwrap_or(ResolvedType::U16),
            fields,
        })
    }

    /// The declared `tag:` entry's own type -- an integer, and only ever the
    /// header's first entry (every variant's leading argument is its value,
    /// so it has nowhere else to sit).
    fn resolve_tag_type(&mut self, field: &HirParam, position: usize) -> Option<ResolvedType> {
        if position != 0 {
            self.error(field.id, field.span, AnalysisErrorKind::EnumTagNotFirst);
            return None;
        }
        let tag_type = self.resolve_type_or_error(field.id, field.span, &field.r#type, true)?;
        if !matches!(tag_type.numeric_kind(), Some(NumericKind::Signed(_) | NumericKind::Unsigned(_))) {
            self.error(field.id, field.span, AnalysisErrorKind::EnumTagNotInteger { found: tag_type });
            return None;
        }
        Some(tag_type)
    }

    /// The shared *dynamic* fields: present on every variant like the header,
    /// but runtime-valued -- so this is header resolution's constant-free,
    /// tag-free sibling. All-or-nothing for the same reason the header is:
    /// the variant loop checks its own field names against this list.
    fn resolve_enum_dynamic_fields(
        &mut self,
        e: &HirEnumDef,
        header: &EnumHeader,
    ) -> Option<Vec<(Ident, ResolvedType, Visibility)>> {
        let mut ok = true;
        let mut fields: Vec<(Ident, ResolvedType, Visibility)> = Vec::new();
        let mut seen: HashMap<Ident, Span> = HashMap::new();

        for field in &e.dynamic_fields {
            if header.claims(&field.ident) || seen.contains_key(&field.ident) {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision { field: field.ident.clone(), variant: None },
                );
                ok = false;
                continue;
            }
            seen.insert(field.ident.clone(), field.span);
            // Laid out inline in every enum value, exactly like a header
            // field -- the same `indirect = false` a struct field passes.
            let Some(resolved) = self.resolve_type_or_error(field.id, field.span, &field.r#type, false) else {
                ok = false;
                continue;
            };
            fields.push((field.ident.clone(), resolved, field.visibility));
        }

        ok.then_some(fields)
    }

    /// Every variant: its tag (unique across the enum), its constant header
    /// values, and its own body fields.
    fn resolve_enum_variants(
        &mut self,
        e: &HirEnumDef,
        header: &EnumHeader,
        dynamic_fields: &[(Ident, ResolvedType, Visibility)],
    ) -> Option<Vec<ResolvedEnumVariant>> {
        let mut ok = true;
        let mut variants: Vec<ResolvedEnumVariant> = Vec::new();
        let mut seen_variants: HashMap<Ident, Span> = HashMap::new();
        let mut seen_tags: HashMap<i128, (Ident, Span)> = HashMap::new();

        for (declared_index, variant) in e.variants.iter().enumerate() {
            if let Some(previous) = seen_variants.insert(variant.name.clone(), variant.span) {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::Redeclaration { name: variant.name.clone(), previous: Some(previous) },
                );
                ok = false;
                continue;
            }

            let expected_args = header.fields.len() + header.has_tag as usize;
            if variant.args.len() != expected_args {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::EnumVariantArgCount {
                        variant: variant.name.clone(),
                        expected: expected_args,
                        found: variant.args.len(),
                        has_tag: header.has_tag,
                    },
                );
                ok = false;
                continue;
            }

            let Some(tag) = self.resolve_variant_tag(variant, header, declared_index) else {
                ok = false;
                continue;
            };
            let tag_key = match tag {
                NumberValue::Signed(value) => value as i128,
                NumberValue::Unsigned(value) => value as i128,
                NumberValue::Float(_) => unreachable!("tag types are integers"),
            };
            if let Some((previous_variant, previous)) = seen_tags.get(&tag_key) {
                self.error(
                    variant.id,
                    variant.span,
                    AnalysisErrorKind::DuplicateEnumTag {
                        variant: variant.name.clone(),
                        value: tag_key.to_string(),
                        previous_variant: previous_variant.clone(),
                        previous: *previous,
                    },
                );
                ok = false;
                continue;
            }
            seen_tags.insert(tag_key, (variant.name.clone(), variant.span));

            // One constant per header field, positionally.
            let mut header_values = Vec::with_capacity(header.fields.len());
            let mut variant_ok = true;
            for ((_, field_type, _), arg) in header.fields.iter().zip(&variant.args[header.has_tag as usize..]) {
                match self.const_eval(arg, field_type) {
                    Some(value) => header_values.push(value),
                    None => variant_ok = false,
                }
            }

            let fields = self.resolve_variant_fields(variant, header, dynamic_fields, &mut variant_ok);
            if !variant_ok {
                ok = false;
                continue;
            }
            variants.push(ResolvedEnumVariant { name: variant.name.clone(), tag, header_values, fields });
        }

        ok.then_some(variants)
    }

    /// One variant's tag: its leading argument when the enum declares an
    /// explicit `tag:`, otherwise its own declared position (`u16`, counting
    /// from 0 -- guaranteed in range, since `u16::MAX` variants is far past
    /// any real declaration).
    fn resolve_variant_tag(
        &mut self,
        variant: &omega_hir::HirEnumVariant,
        header: &EnumHeader,
        declared_index: usize,
    ) -> Option<NumberValue> {
        if !header.has_tag {
            return Some(NumberValue::Unsigned(declared_index as u64));
        }
        match self.const_eval(&variant.args[0], &header.tag_type)? {
            ConstValue::Number(value) => Some(value),
            _ => unreachable!("const_eval only produces Number for an integer expected type"),
        }
    }

    /// One variant's own body fields. They must not collide with the header,
    /// the shared dynamic fields, or the reserved `tag` -- all three are
    /// reached through the same `value.name` syntax.
    fn resolve_variant_fields(
        &mut self,
        variant: &omega_hir::HirEnumVariant,
        header: &EnumHeader,
        dynamic_fields: &[(Ident, ResolvedType, Visibility)],
        ok: &mut bool,
    ) -> Vec<(Ident, ResolvedType, Visibility)> {
        let mut fields: Vec<(Ident, ResolvedType, Visibility)> = Vec::new();
        let mut seen: HashMap<Ident, Span> = HashMap::new();

        for field in &variant.fields {
            let shadows_shared =
                header.claims(&field.ident) || dynamic_fields.iter().any(|(name, _, _)| *name == field.ident);
            if shadows_shared {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::EnumFieldNameCollision {
                        field: field.ident.clone(),
                        variant: Some(variant.name.clone()),
                    },
                );
                *ok = false;
                continue;
            }
            if let Some(previous) = seen.insert(field.ident.clone(), field.span) {
                self.error(
                    field.id,
                    field.span,
                    AnalysisErrorKind::Redeclaration { name: field.ident.clone(), previous: Some(previous) },
                );
                *ok = false;
                continue;
            }
            // A body field is inline layout, exactly like a struct field --
            // the one context that catches by-value recursion.
            let Some(resolved) = self.resolve_type_or_error(field.id, field.span, &field.r#type, false) else {
                *ok = false;
                continue;
            };
            fields.push((field.ident.clone(), resolved, field.visibility));
        }
        fields
    }

    /// A method sharing a variant's name would make `Enum::name` ambiguous --
    /// rejected outright, before any signature is collected.
    fn check_variant_name_collisions(&mut self, e: &HirEnumDef) -> bool {
        let mut ok = true;
        let mut variants: HashMap<&Ident, Span> = HashMap::new();
        for variant in &e.variants {
            variants.entry(&variant.name).or_insert(variant.span);
        }
        for function in &e.functions {
            if let Some(previous) = variants.get(&function.name) {
                self.error(
                    function.id,
                    function.span,
                    AnalysisErrorKind::Redeclaration { name: function.name.clone(), previous: Some(*previous) },
                );
                ok = false;
            }
        }
        ok
    }

    /// An enum's tag, header, dynamic fields, variants, and methods.
    pub fn signature_of_enum(
        &mut self,
        e: &HirEnumDef,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        method_ids: &[HirId],
    ) -> Option<Vec<PendingSpecMethod>> {
        let annotations = self.item_annotations(e.id, &e.annotations, crate::annotations::ItemKind::Enum);
        cell.borrow_mut().layout = annotations.layout;
        cell.borrow_mut().suppress = annotations.suppress;

        let header = self.resolve_enum_header(e)?;
        let dynamic_fields = self.resolve_enum_dynamic_fields(e, &header)?;
        // Both of these run even when the other fails: a variant's own
        // errors and a variant/method name collision are independent
        // findings, and reporting only the first would hide the rest.
        let variants = self.resolve_enum_variants(e, &header, &dynamic_fields);
        let names_ok = self.check_variant_name_collisions(e);
        let variants = variants.filter(|_| names_ok)?;

        {
            let mut resolved = cell.borrow_mut();
            resolved.tag_type = header.tag_type;
            resolved.header = header.fields;
            resolved.dynamic_fields = dynamic_fields;
            resolved.variants = variants;
        }

        let self_type = ResolvedType::Enum { cell: cell.clone(), variant: None };
        let (functions, pending, implemented_specs) = self.collect_methods(
            (e.id, e.span),
            &e.name,
            &e.functions,
            &e.implements,
            method_ids,
            &self_type,
            false, // `@glue` only applies to markers -- see `ItemKind::Spec`'s doc comment.
        )?;
        cell.borrow_mut().functions = functions;
        cell.borrow_mut().implemented_specs = implemented_specs;
        Some(pending)
    }

    /// Checks a top-level enum's function *bodies* only -- the counterpart
    /// of `check_struct_body`, with the same read-back-from-the-cell
    /// discipline (see its doc comment); an enum's fields/variants have no
    /// body work of their own (their values were fully evaluated during
    /// `signature_of_enum`).
    /// Checks a function's (or method's) *body* only -- its signature *and*
    /// its resolved annotations, along with its own name bound so any call
    /// to it (including a recursive one from its own body) resolves, are
    /// already handled by `omega_driver::Driver::ensure_item`/
    /// `collect_function_signature`. Enters a fresh scope to bind `f`'s
    /// params by name (signature collection only ever resolved their
    /// *types*, never bound them -- that's a body-analysis-time concern,
    /// same as it always was).
    /// `id` is stamped onto the produced `CheckedFunctionDef` in place of
    /// always reading `f.id` -- for an ordinary (non-generic) function this
    /// is just `f.id` (behavior-preserving); for a generic instantiation
    /// it's the same freshly-minted synthetic id `omega_driver::Driver`
    /// already decided (and stored) during the signature phase, so codegen
    /// gets one distinct compiled function per instantiation.
    /// `annotations` is read back, never re-resolved -- `collect_function_
    /// signature` already resolved it once, at signature time, which is
    /// also what makes it visible to an extern-owned function/method whose
    /// body this compilation never checks at all (see
    /// `collect_function_signature`'s own doc comment).
    /// Discovers the one concrete return type a `spec Bound<...>` (static-
    /// dispatch, no `*`) return-type function's own body implies -- the
    /// throwaway "pass 1" `omega_driver::Driver::resolve_spec_return_function`
    /// runs before this function's real signature can be finalized at all
    /// (there is no concrete `ResolvedType` to give `collect_function_
    /// signature` otherwise). Binds params exactly like `check_function_body`
    /// does (params never read `current_return_type`, so this is safe to do
    /// without knowing the return type yet), then walks the body with
    /// `inferring_return_type` set so every exit point's own resolved type is
    /// merely *recorded* (see `HirStmt::Return`'s arm), never checked against
    /// anything -- there is nothing to check against yet.
    ///
    /// Every candidate must be the exact same concrete `ResolvedType` --
    /// Rust's `impl Trait` rule (one concrete type across the whole
    /// function), not "each individually satisfies the bound" -- and that
    /// unified type must itself implement `bound_spec<bound_type_args>`.
    /// This method's own errors are only ever genuine ones (an ambiguity, an
    /// unconstrained body, or a bound violation, on top of whatever ordinary
    /// body-analysis already reports) -- the caller discards them on success
    /// and keeps them only on failure, since a second, real pass
    /// (`check_function_body`, once this concrete type is known) is what
    /// actually gets cached and used everywhere.
    pub fn infer_body_return_type(&mut self, f: &HirFunctionDef, bound: &Type) -> Option<ResolvedType> {
        self.context.enter_scope();
        self.analyze_all(&f.params, Self::analyze_param);

        self.current_return_type = ResolvedType::Void;
        self.loop_stack.clear();
        self.in_defer_body = false;
        self.inferring_return_type = true;
        self.inferred_return_candidates.clear();
        let body = self.analyze_block(&f.body, None);
        self.inferring_return_type = false;

        let scope = self.context.leave_scope();
        self.warn_unused_bindings(scope, true);

        let body = body?;
        if let Some(tail_type) = Self::block_type(&body) {
            self.inferred_return_candidates.push((f.span, tail_type));
        }

        let mut candidates = std::mem::take(&mut self.inferred_return_candidates);
        if candidates.is_empty() {
            self.error(f.id, f.span, AnalysisErrorKind::SpecReturnTypeUnconstrained { function: f.name.clone() });
            return None;
        }
        let (_, first) = candidates.remove(0);
        for (span, candidate) in candidates {
            if candidate != first {
                self.error(
                    f.id,
                    span,
                    AnalysisErrorKind::AmbiguousSpecReturnType {
                        function: f.name.clone(),
                        first: first.clone(),
                        second: candidate,
                    },
                );
                return None;
            }
        }
        match self.check_generic_bound(f.id, f.span, bound, &first) {
            // `bound` itself failed to resolve -- already an ordinary
            // recorded error (see `check_generic_bound`'s own doc comment).
            None => None,
            Some(Ok(())) => Some(first),
            Some(Err((spec, missing))) => {
                self.error(
                    f.id,
                    f.span,
                    AnalysisErrorKind::SpecReturnTypeNotSatisfied { function: f.name.clone(), r#type: first, spec, missing },
                );
                None
            }
        }
    }

    pub fn check_function_body(
        &mut self,
        f: &HirFunctionDef,
        fn_type: &ResolvedFunctionType,
        id: HirId,
        annotations: &crate::annotations::ResolvedAnnotations,
    ) -> Option<CheckedFunctionDef> {
        self.suppressed.push(annotations.suppress.clone());
        if annotations.inline.is_some() {
            self.warn(f.id, f.span, AnalysisWarningKind::InlineNotEnforced);
        }

        self.context.enter_scope();
        let params = self.analyze_all(&f.params, Self::analyze_param);

        // One `Analyzer` checks exactly one top-level item at a time (see
        // `item_name`'s doc comment), and a struct's methods are checked
        // sequentially, never while another method/function's body is still
        // being analyzed -- so there's no *nesting* to protect against here,
        // just an ordinary reset before each independent body: no enclosing
        // loop or defer of its own, and its own declared return type.
        self.current_return_type = (*fn_type.return_type).clone();
        self.loop_stack.clear();
        self.in_defer_body = false;
        // The function's own declared return type is the expected type for
        // an implicit tail-expression return (`fn f() => f64 { 10 }`) --
        // the same untyped-constant adaptation an explicit `return 10;`
        // gets (see `HirStmt::Return`'s arm above).
        let body = self.analyze_block(&f.body, Some(fn_type.return_type.as_ref()));

        let scope = self.context.leave_scope();
        self.warn_unused_bindings(scope, true);
        self.suppressed.pop();

        let params = params?;
        let body = body?;
        self.check_function_return(f.id, f.span, &fn_type.return_type, &body)?;

        Some(CheckedFunctionDef {
            id,
            span: f.span,
            name: f.name.clone(),
            type_args: vec![],
            self_mode: f.self_mode,
            is_variadic: false,
            params,
            return_type: (*fn_type.return_type).clone(),
            body,
            inline: annotations.inline,
            mangling: annotations.mangling.clone(),
            extension_target: None,
        })
    }

    /// Checks one queued default-method instantiation's body (see
    /// `PendingSpecMethod`) -- reconstructs an ordinary, synthetic
    /// `HirFunctionDef` straight out of the spec's own raw signature (see
    /// `RawSpecFunctionSig`'s doc comment for why it carries real
    /// `HirParam`s, not just names/types) and reuses `check_function_body`
    /// wholesale, rather than duplicating its param-binding/return-checking
    /// logic. The caller (`omega_driver::Driver::check_item_body`)
    /// constructs `self` fresh, seeded with exactly `pending.substitution`
    /// (`Self` + the owning spec's own generics) -- the implementor's own
    /// generics are never relevant here, since the spec's HIR can't
    /// reference a name it doesn't know about.
    pub fn check_pending_spec_method(&mut self, pending: &PendingSpecMethod) -> Option<CheckedFunctionDef> {
        // A spec-satisfying default method effectively becomes part of its
        // implementor's own definition -- same field-access rights as any
        // of that type's own hand-written methods (see `Analyzer::
        // current_owner`'s doc comment). `Self` was already seeded into
        // this `Analyzer`'s own scope by `Analyzer::new` (from `pending`'s
        // own substitution, via `omega_driver::Driver::
        // check_pending_spec_methods`) -- looked back up here rather than
        // threaded as a separate parameter, since it's already the single
        // source of truth for what `Self` resolves to in this pending
        // method's own body. `None` (no case matched) for a primitive
        // `Self` (a `for`-attached spec target) -- primitives have no
        // fields to protect this way at all.
        self.current_owner = match self.context.find_defined_type(&Ident("Self".to_string())) {
            Some(ResolvedType::Struct(cell)) => Some(cell.borrow().id),
            Some(ResolvedType::Union(cell)) => Some(cell.borrow().id),
            Some(ResolvedType::Enum { cell, .. }) => Some(cell.borrow().id),
            _ => None,
        };
        let body = pending
            .raw
            .default_body
            .clone()
            .expect("only ever queued (resolve_implements_clause) when a default body exists");
        let synthetic = HirFunctionDef {
            id: pending.raw.decl_id,
            span: pending.raw.span,
            // Spec default methods carry no annotations of their own -- not
            // yet part of the language's spec-function grammar (see
            // `omega_analyzer::annotations`'s doc comment).
            annotations: Vec::new(),
            // Same reasoning as `annotations` above -- a spec function has
            // no visibility modifier of its own (see `ResolvedMethod`'s
            // `Visibility::Exposed` default at the two spec-flattening call
            // sites); this synthetic body-check `HirFunctionDef` is never
            // read through `item_visibility` (it's not a real top-level
            // item), so the value here is never actually consulted.
            visibility: Visibility::default(),
            name: pending.raw.name.clone(),
            generics: vec![],
            self_mode: pending.raw.self_mode,
            params: pending.raw.params.clone(),
            return_type: pending.raw.return_type.clone(),
            body,
        };
        self.check_function_body(
            &synthetic,
            &pending.fn_type,
            pending.id,
            &crate::annotations::ResolvedAnnotations::default(),
        )
    }
    /// Every method body of one aggregate, checked with the aggregate's own
    /// `@suppress` frame active.
    ///
    /// `methods` is read back off the cell, never re-derived: a generic
    /// instantiation's methods must get the exact same synthetic ids the
    /// signature phase already decided for them.
    fn check_method_bodies(
        &mut self,
        functions: &[omega_hir::HirFunctionDef],
        methods: &[(Ident, ResolvedMethod)],
        suppress: &[Ident],
    ) -> Option<Vec<CheckedFunctionDef>> {
        self.suppressed.push(suppress.to_vec());
        self.context.enter_scope();
        let mut checked = Vec::with_capacity(functions.len());
        let mut ok = true;
        for (f, (_, method)) in functions.iter().zip(methods) {
            match self.check_function_body(f, &method.fn_type, method.decl_id, &method.annotations) {
                Some(body) => checked.push(body),
                // Every method is checked even after one fails, so a broken
                // method never hides its siblings' own errors.
                None => ok = false,
            }
        }
        self.context.leave_scope();
        self.suppressed.pop();
        ok.then_some(checked)
    }

    /// One aggregate's checked field list, pairing each declared field with
    /// the type the signature phase already resolved for it.
    fn checked_fields(declared: &[HirParam], resolved: &[(Ident, ResolvedType, Visibility)]) -> Vec<CheckedParam> {
        declared
            .iter()
            .zip(resolved)
            .map(|(field, (_, r#type, _))| CheckedParam {
                id: field.id,
                span: field.span,
                ident: field.ident.clone(),
                r#type: r#type.clone(),
            })
            .collect()
    }

    /// Checks a top-level struct's method bodies. Its fields have no body
    /// work of their own -- they were fully resolved at signature time -- so
    /// they are only carried through into the checked tree here.
    pub fn check_struct_body(
        &mut self,
        s: &HirStructDef,
        cell: &Rc<RefCell<ResolvedStructType>>,
    ) -> Option<CheckedStructDef> {
        let (fields, methods, suppress) = {
            let resolved = cell.borrow();
            self.current_owner = Some(resolved.id);
            (Self::checked_fields(&s.fields, &resolved.fields), resolved.functions.clone(), resolved.suppress.clone())
        };
        let functions = self.check_method_bodies(&s.functions, &methods, &suppress)?;
        Some(CheckedStructDef { id: s.id, span: s.span, name: s.name.clone(), type_args: vec![], fields, functions })
    }

    /// A union's bodies -- identical contract to `check_struct_body`.
    pub fn check_union_body(
        &mut self,
        u: &HirUnionDef,
        cell: &Rc<RefCell<ResolvedUnionType>>,
    ) -> Option<CheckedUnionDef> {
        let (fields, methods, suppress) = {
            let resolved = cell.borrow();
            self.current_owner = Some(resolved.id);
            (Self::checked_fields(&u.fields, &resolved.fields), resolved.functions.clone(), resolved.suppress.clone())
        };
        let functions = self.check_method_bodies(&u.functions, &methods, &suppress)?;
        Some(CheckedUnionDef { id: u.id, span: u.span, name: u.name.clone(), type_args: vec![], fields, functions })
    }

    /// An enum's bodies. Unlike a struct or union, an enum's fields and
    /// variants carry no checked form of their own at all -- their values
    /// were fully evaluated during `signature_of_enum`.
    pub fn check_enum_body(&mut self, e: &HirEnumDef, cell: &Rc<RefCell<ResolvedEnumType>>) -> Option<CheckedEnumDef> {
        let (methods, suppress) = {
            let resolved = cell.borrow();
            self.current_owner = Some(resolved.id);
            (resolved.functions.clone(), resolved.suppress.clone())
        };
        let functions = self.check_method_bodies(&e.functions, &methods, &suppress)?;
        Some(CheckedEnumDef { id: e.id, span: e.span, name: e.name.clone(), type_args: vec![], functions })
    }
}
