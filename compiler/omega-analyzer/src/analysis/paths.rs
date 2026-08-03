use super::*;

impl<'r> Analyzer<'r> {
    /// What `alias` means as an import in this module, resolved lazily and
    /// memoized by the driver per `(module_path, alias)` pair -- `Ok(None)`
    /// means this module has no `import` statement binding `alias` at all,
    /// the signal every caller's own "assume this is my own module's item"
    /// fallback keys off. This is the direct replacement for the old
    /// `Context::absolute_path`/`generic_alias`/`bind_imported_item`, which
    /// used to be populated eagerly, for a module's *entire* import list,
    /// before any item in it was ever touched -- see `Analyzer::new`'s doc
    /// comment for why that was a real false-cycle bug, not just eagerness.
    pub(super) fn resolve_alias(&mut self, alias: &Ident) -> Result<Option<ImportTarget>, ResolveError> {
        self.resolver.resolve_import_alias(&self.module_path, alias)
    }

    /// `resolve_alias`, with a real resolution failure (a cycle, a broken
    /// target module, ...) folded directly into `self.errors` -- the
    /// `Option<Option<_>>` "handled or fall through" shape every *hard*
    /// (non-probing) call site wants: outer `None` means an error was
    /// already pushed and the caller should give up immediately (`?`);
    /// `Some(None)` means `alias` isn't an import at all, the caller's own
    /// fallback applies.
    pub(super) fn resolve_alias_or_error(&mut self, node_id: HirId, span: Span, alias: &Ident) -> Option<Option<ImportTarget>> {
        match self.resolve_alias(alias) {
            Ok(target) => Some(target),
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                None
            }
        }
    }

    /// The alias (of any kind -- module, item, or generic item) this
    /// module's own `import` statements bind that's most similar to
    /// `target` -- the "did you mean" suggestion for a reference that named
    /// nothing at all. Replaces `Context`'s old `similar_module_alias`
    /// (which only ever knew about whole-module aliases, pre-populated
    /// eagerly); `ModuleResolver::import_alias_names` is the only remaining
    /// place that knows a module's whole alias set up front, since
    /// resolving what each one actually *means* is lazy now.
    pub(super) fn similar_import_alias(&mut self, target: &Ident) -> Option<Ident> {
        best_match(target, self.resolver.import_alias_names(&self.module_path).iter())
    }

    /// Resolves `absolute` (already a full `[module_path.., name]`, whether
    /// built from a qualified place's import alias or an unqualified one's
    /// implicit own-module prefix) to a place root -- shared by both of
    /// `analyze_place`'s non-local cases so the `Value`/`Type`/`Err` match
    /// is only written once.
    /// `unqualified` is the bare name the user actually wrote, when this
    /// query is the implicit own-module fallback for one -- an
    /// `UnknownItem` miss then means "no such variable", and is reported as
    /// exactly that (with a typo suggestion from the visible scopes) rather
    /// than as a confusing module-shaped error about the module the user
    /// never mentioned.
    pub(super) fn resolve_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        absolute: Vec<Ident>,
        unqualified: Option<&Ident>,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType, bool)> {
        // A bare (uncalled) reference to an overloaded name -- `resolve_item`
        // would otherwise silently resolve it to whichever candidate the
        // driver happens to index first (see `ModuleResolver::resolve_item`'s
        // single-result contract, which has no way to pick one at all). A
        // call site (`resolve_overloaded_call`) has the argument types
        // needed to disambiguate; anywhere else, the only other thing that
        // can disambiguate is an explicit function-typed `expected` (a
        // declaration/assignment annotation) that structurally matches
        // exactly one candidate's signature -- everything else is
        // unconditionally ambiguous, reported with every candidate listed
        // and no winner.
        if let Some((name, module_path)) = absolute.split_last()
            && let Ok(Some(candidates)) = self.resolver.function_overload_signatures(module_path, name)
        {
            let signatures: Vec<(HirId, ResolvedFunctionType)> =
                candidates.iter().map(|(id, fn_type, _)| (*id, fn_type.clone())).collect();
            if let Some(ResolvedType::Function(expected_fn)) = expected
                && let Some((decl_id, fn_type)) = Self::unique_overload_signature_match(expected_fn, &signatures)
            {
                // Same post-winner visibility check as `resolve_overloaded_
                // call`'s identical situation -- structural signature
                // matching (here, against `expected`) has no notion of
                // visibility either.
                let visibility = candidates
                    .iter()
                    .find(|(id, ..)| *id == decl_id)
                    .map(|(_, _, v)| *v)
                    .expect("decl_id came from this same candidates list");
                if !self.check_visibility(visibility, module_path) {
                    self.error(
                        node_id,
                        span,
                        AnalysisErrorKind::ModuleResolution(ResolveError::NotVisible {
                            module: module_path.to_vec(),
                            item: name.clone(),
                        }),
                    );
                    return None;
                }
                let r#type = ResolvedType::Function(fn_type);
                let root = CheckedPlaceRoot::Variable { decl_id, storage: Storage::Function, r#type: r#type.clone() };
                return Some((root, r#type, false));
            }
            self.error(
                node_id,
                span,
                AnalysisErrorKind::AmbiguousOverload {
                    name: name.clone(),
                    candidates: candidates.into_iter().map(|(_, t, _)| t).collect(),
                },
            );
            return None;
        }
        match self.resolve_item_checked(&absolute, &[], true) {
            Ok(ResolvedItem::Value { r#type, storage, decl_id, mutable }) => {
                let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                Some((root, r#type, mutable))
            }
            Ok(ResolvedItem::Type(_)) => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Err(ResolveError::UnknownItem { .. }) if unqualified.is_some() => {
                let name = unqualified.expect("checked by the guard").clone();
                // Scope-level candidates first, then this module's own
                // top-level values (functions/globals/externs) -- only the
                // resolver holds a module-wide name list.
                let similar = self.context.similar_variable_name(&name).or_else(|| {
                    self.resolver.similar_item_name(&self.module_path, &name, ItemNamespace::Value)
                });
                self.error(node_id, span, AnalysisErrorKind::UndefinedVariable { name, similar });
                None
            }
            // `mymodule::MyStruct::do_thing` -- the "module" that failed to
            // resolve (`mymodule::MyStruct`) may actually be a struct, and
            // the last segment one of its static functions. Only attempted
            // when the missing module is exactly this path minus its last
            // segment (a deeper miss can't be this shape).
            Err(ResolveError::UnknownModule(missing))
                if missing.len() + 1 == absolute.len() && missing == absolute[..missing.len()] =>
            {
                match self.resolve_item_checked(&missing, &[], true) {
                    Ok(ResolvedItem::Type(t)) => self
                        .resolve_type_member(node_id, span, &t, &absolute[missing.len()..])
                        .map(|(root, r#type)| (root, r#type, false)),
                    _ => {
                        self.error(
                            node_id,
                            span,
                            AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(missing)),
                        );
                        None
                    }
                }
            }
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                None
            }
        }
    }

    /// Finds the one candidate (if any) among an overloaded name's
    /// signatures that structurally matches `expected` -- a function-typed
    /// declaration/assignment annotation naming exactly which overload is
    /// meant (`f : (a: u64) => void = f;`). Compared by shape only (param
    /// types in order, return type, `is_variadic`/`self_mode`),
    /// never by parameter name -- the annotation's own parameter names have
    /// no reason to match the target function's, same "types only" spirit
    /// as `check_overload_duplicates`'s pairwise comparison. Zero or 2+
    /// matches both return `None`: a real duplicate overload set is already
    /// rejected elsewhere (`check_overload_duplicates`), so 2+ here would
    /// mean the annotation itself is ambiguous, not that a choice exists.
    pub(super) fn unique_overload_signature_match(
        expected: &ResolvedFunctionType,
        candidates: &[(HirId, ResolvedFunctionType)],
    ) -> Option<(HirId, ResolvedFunctionType)> {
        let mut matches = candidates.iter().filter(|(_, fn_type)| {
            fn_type.is_variadic == expected.is_variadic
                && fn_type.self_mode == expected.self_mode
                && fn_type.return_type == expected.return_type
                && fn_type.params.len() == expected.params.len()
                && fn_type.params.iter().zip(&expected.params).all(|((_, a), (_, b))| a == b)
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.clone())
    }

    /// `Head::function` where `Head` isn't an imported module alias -- the
    /// head may instead name a struct *type* (a builtin/imported/locally
    /// defined one via `find_defined_type`, or this module's own top-level
    /// struct via the resolver), making this a static-function reference.
    /// Reports the most precise error it can when the head names nothing
    /// usable -- `ModuleNotImported` only when the head is genuinely
    /// unknown, never when it exists but is the wrong kind of thing (a
    /// wrong "add `import ...;`" hint would be worse than none).
    pub(super) fn resolve_type_qualified_value(
        &mut self,
        node_id: HirId,
        span: Span,
        path: &omega_parser::prelude::Path,
        expected: Option<&ResolvedType>,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        // `str` is deliberately absent from `defined_types` (the `*str`
        // feature's own invariant -- see `Context::resolve_type`'s doc
        // comment), so `str::from_bytes_unchecked(...)`-style static calls
        // on a `for str` extension spec need this one narrow carve-out to
        // even reach `resolve_type_member` at all.
        if path.head.as_ref() == "str" {
            return self.resolve_type_member(node_id, span, &ResolvedType::Str { mutable: false }, &path.tail);
        }
        if let Some(head_type) = self.context.find_defined_type(&path.head).cloned() {
            return self.resolve_type_member(node_id, span, &head_type, &path.tail);
        }

        // A plain (non-generic) *type* import alias resolves outright,
        // exactly the same lazy-alias treatment `Context::resolve_type`
        // gives an unqualified `Type::Named` -- see its own comment for why
        // this can no longer be caught by `find_defined_type` above.
        let alias = self.resolve_alias_or_error(node_id, span, &path.head)?;
        if let Some(ImportTarget::Item(_, ResolvedItem::Type(t))) = alias {
            return self.resolve_type_member(node_id, span, &t, &path.tail);
        }
        let absolute: Vec<Ident> = match alias {
            Some(ImportTarget::GenericItem(absolute)) | Some(ImportTarget::Module(absolute)) => absolute,
            _ => self.module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect(),
        };
        // A bare reference to a generic enum's unit variant (`Option::
        // None`, no `{ }` at all -- so no field values to unify against,
        // unlike a literal) can still be inferred from an `expected`
        // (surrounding-context) type -- see `infer_literal_type_args`,
        // called here with no fields.
        let variant = path.tail.first();
        let result = match self.generic_literal_signature_with_ambient(std::slice::from_ref(&path.head), &absolute, variant) {
            Some((real_absolute, sig)) => {
                let type_args = self.infer_literal_type_args(node_id, span, &real_absolute, &sig, &[], expected)?;
                self.resolve_item_checked_with_ambient_fallback(std::slice::from_ref(&path.head), &real_absolute, &type_args)
            }
            None => self.resolve_item_checked_with_ambient_fallback(std::slice::from_ref(&path.head), &absolute, &[]),
        };
        let kind = match result {
            Ok(ResolvedItem::Type(t)) => {
                return self.resolve_type_member(node_id, span, &t, &path.tail);
            }
            Ok(ResolvedItem::Value { .. }) => AnalysisErrorKind::NotAModule { name: path.head.clone() },
            // The head names nothing at all -- an unimported module, or a
            // typo of a struct/module that does exist; suggest whichever
            // actually does.
            Err(ResolveError::UnknownItem { .. }) => AnalysisErrorKind::UndefinedPathHead {
                name: path.head.clone(),
                similar_module: self.similar_import_alias(&path.head),
                similar_type: self.context.similar_type_name(&path.head).or_else(|| {
                    self.resolver.similar_item_name(&self.module_path, &path.head, ItemNamespace::Type)
                }),
            },
            // The head *does* name something here (a failed item, an
            // uninstantiated generic, ...) -- report that, precisely.
            Err(e) => AnalysisErrorKind::ModuleResolution(e),
        };
        self.error(node_id, span, kind);
        None
    }

    /// A place root whose path carries explicit generic arguments
    /// (`Optional<u32>::Some`, `List<u8>::new`, `sum_generic<f64>`): the
    /// argumented prefix resolves through the same instantiating
    /// `resolve_item` query every other generic reference uses, and
    /// whatever one segment may follow it resolves as a member of the
    /// resulting type (`resolve_type_member`). An instantiated *value* (a
    /// generic function referenced with explicit arguments) is legal only
    /// with nothing after it.
    pub(super) fn resolve_generic_args_place(
        &mut self,
        node_id: HirId,
        span: Span,
        expr_path: &ExprPath,
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let segments = expr_path.path.segments();
        let rest = &segments[expr_path.args_at + 1..];
        if rest.len() > 1 {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::GenericPathTooDeep { r#type: segments[expr_path.args_at].clone() },
            );
            return None;
        }

        let type_args = self.resolve_generic_arg_list(node_id, span, expr_path)?;
        let prefix = &segments[..=expr_path.args_at];
        let absolute = self.generic_prefix_absolute(node_id, span, prefix)?;
        match self.resolve_item_checked_with_ambient_fallback(prefix, &absolute, &type_args) {
            Ok(ResolvedItem::Type(_)) if rest.is_empty() => {
                self.error(node_id, span, AnalysisErrorKind::NotAValue(absolute));
                None
            }
            Ok(ResolvedItem::Type(t)) => self.resolve_type_member(node_id, span, &t, rest),
            Ok(ResolvedItem::Value { r#type, storage, decl_id, mutable: _ }) if rest.is_empty() => {
                let root = CheckedPlaceRoot::Variable { decl_id, storage, r#type: r#type.clone() };
                Some((root, r#type))
            }
            Ok(ResolvedItem::Value { .. }) => {
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::NotAModule { name: segments[expr_path.args_at].clone() },
                );
                None
            }
            Err(e) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(e));
                None
            }
        }
    }

    /// Resolves an `ExprPath`'s written `<T, ...>` arguments -- always
    /// indirect, same reasoning as `Type::Generic`'s argument resolution in
    /// `Context::resolve_type`.
    pub(super) fn resolve_generic_arg_list(&mut self, node_id: HirId, span: Span, expr_path: &ExprPath) -> Option<Vec<ResolvedType>> {
        self.analyze_all(&expr_path.generic_args, |this, arg| {
            this.resolve_type_or_error(node_id, span, arg, true)
        })
    }

    /// The absolute item path of an expression path's generic-argumented
    /// *prefix* (`Optional` in `Optional<u32>::Some`, `mymodule::List` in
    /// `mymodule::List<u8>::new`) -- the same alias-vs-own-module priority
    /// `Context::resolve_absolute_item_path` applies to type positions.
    pub(super) fn generic_prefix_absolute(&mut self, node_id: HirId, span: Span, prefix: &[Ident]) -> Option<Vec<Ident>> {
        if let [single] = prefix {
            if let Some(ImportTarget::GenericItem(absolute)) = self.resolve_alias_or_error(node_id, span, single)? {
                return Some(absolute);
            }
            return Some(self.module_path.iter().cloned().chain(std::iter::once(single.clone())).collect());
        }
        let path = omega_parser::prelude::Path { head: prefix[0].clone(), tail: prefix[1..].to_vec() };
        match self.resolve_alias_or_error(node_id, span, &path.head)? {
            Some(ImportTarget::Module(target)) => {
                Some(target.into_iter().chain(path.tail.iter().cloned()).collect())
            }
            _ => {
                let similar_module = self.similar_import_alias(&path.head);
                self.error(
                    node_id,
                    span,
                    AnalysisErrorKind::UndefinedPathHead {
                        name: path.head.clone(),
                        similar_module,
                        similar_type: self.context.similar_type_name(&path.head),
                    },
                );
                None
            }
        }
    }

    /// `Type::member` -- resolves `rest` (the path segments after the type's
    /// own name, always non-empty) against `r#type`'s members. For a struct
    /// that can only be a static function; for an enum it's a variant
    /// (producing a whole constructed value -- the unit form, so only valid
    /// for a body-less variant) or a static function. A function declared
    /// *without* `self` is static: callable through the type's name alone,
    /// with no instance. A static function resolves to an ordinary
    /// `Storage::Function` place root, exactly what a member-call callee
    /// resolves to; a unit variant resolves to a `CheckedPlaceRoot::Expr`
    /// construction -- codegen needs no new machinery for either.
    fn resolve_type_member(
        &mut self,
        node_id: HirId,
        span: Span,
        r#type: &ResolvedType,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        let member = &rest[0];
        let (type_name, method, missing_member_error, owner_module_path, owner_id) = match r#type {
            ResolvedType::Struct(cell) => {
                let struct_type = cell.borrow();
                let method = struct_type
                    .functions
                    .iter()
                    .find(|(name, _)| name == member)
                    .map(|(_, method)| method.clone());
                let similar = match method {
                    Some(_) => None,
                    None => best_match(member, struct_type.functions.iter().map(|(name, _)| name)),
                };
                let missing = AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: struct_type.name.clone(),
                    function: member.clone(),
                    similar,
                };
                (struct_type.name.clone(), method, missing, struct_type.module_path.clone(), struct_type.id)
            }
            ResolvedType::Union(cell) => {
                let union_type = cell.borrow();
                let method = union_type
                    .functions
                    .iter()
                    .find(|(name, _)| name == member)
                    .map(|(_, method)| method.clone());
                let similar = match method {
                    Some(_) => None,
                    None => best_match(member, union_type.functions.iter().map(|(name, _)| name)),
                };
                let missing = AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: union_type.name.clone(),
                    function: member.clone(),
                    similar,
                };
                (union_type.name.clone(), method, missing, union_type.module_path.clone(), union_type.id)
            }
            ResolvedType::Enum { cell, .. } => {
                // A variant wins over a same-named function -- analysis of
                // the definition would ideally forbid the collision, but
                // resolution still needs a deterministic order.
                let found = cell.borrow().variant(member).map(|(i, v)| (i, v.clone()));
                if let Some((variant_index, variant)) = found {
                    return self.resolve_unit_variant(node_id, span, cell, variant_index, &variant, rest);
                }
                let e = cell.borrow();
                let method = e
                    .functions
                    .iter()
                    .find(|(name, _)| name == member)
                    .map(|(_, method)| method.clone());
                let missing = AnalysisErrorKind::NoSuchEnumMember {
                    r#enum: e.name.clone(),
                    name: member.clone(),
                    similar_variant: best_match(member, e.variants.iter().map(|v| &v.name)),
                    similar_function: best_match(member, e.functions.iter().map(|(name, _)| name)),
                };
                (e.name.clone(), method, missing, e.module_path.clone(), e.id)
            }
            // `GapSpec::function(...)` -- a `@gap` spec's own qualified
            // name is callable exactly as if it were a marker's static
            // function (see `docs/21-gaps-and-glue.md`), resolving directly
            // against its already-resolved `gap_functions` (eagerly typed
            // at the spec's own declaration -- see `GapFunction`'s doc
            // comment -- so there's no `Self`/implementor to wait for the
            // way an ordinary spec's functions would need). A synthetic
            // `ResolvedMethod` lets this share every bit of the shared tail
            // below (visibility, "too many segments", the final
            // `CheckedPlaceRoot`) with the `Struct`/`Union`/`Enum` arms
            // above, rather than duplicating it. A *non*-gap spec falls
            // through to the `other` arm below, completely unchanged --
            // still the ordinary `StaticAccessOnNonStruct`.
            ResolvedType::Spec(cell) if cell.borrow().is_gap => {
                let spec_type = cell.borrow();
                let method = spec_type.gap_functions.iter().find(|(name, _)| name == member).map(|(_, gap_fn)| {
                    ResolvedMethod {
                        decl_id: gap_fn.decl_id,
                        fn_type: gap_fn.fn_type.clone(),
                        visibility: spec_type.visibility,
                        annotations: crate::annotations::ResolvedAnnotations::default(),
                    }
                });
                let similar = match method {
                    Some(_) => None,
                    None => best_match(member, spec_type.gap_functions.iter().map(|(name, _)| name)),
                };
                let missing = AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: spec_type.name.clone(),
                    function: member.clone(),
                    similar,
                };
                (spec_type.name.clone(), method, missing, spec_type.module_path.clone(), spec_type.id)
            }
            // A primitive (or `Slice`/`Str`) has no static members of its
            // own, unless a `for`-attached spec in `core` gave it some (see
            // `HirSpecDef::target`'s doc comment) -- a self-less function
            // reaches call sites this way (`str::from_bytes_unchecked(...)`);
            // an instance one goes through `find_methods` instead.
            other => {
                let methods = match self.resolver.extension_methods(other) {
                    Ok(methods) => methods,
                    Err(err) => {
                        self.error(node_id, span, AnalysisErrorKind::ModuleResolution(err));
                        return None;
                    }
                };
                if methods.is_empty() {
                    self.error(node_id, span, AnalysisErrorKind::StaticAccessOnNonStruct { found: other.clone() });
                    return None;
                }
                let type_name = Ident(other.to_string());
                let method = methods.iter().find(|(name, _)| name == member).map(|(_, m)| m.clone());
                let similar = match method {
                    Some(_) => None,
                    None => best_match(member, methods.iter().map(|(name, _)| name)),
                };
                let missing = AnalysisErrorKind::NoSuchStructFunction {
                    r#struct: type_name.clone(),
                    function: member.clone(),
                    similar,
                };
                // A primitive extension method is always `Exposed` (see
                // `resolve_extension_methods`) -- the empty path/`node_id`
                // placeholder here are never actually consulted by
                // `check_member_visibility` (only reached for `Hidden`).
                (type_name, method, missing, Vec::new(), node_id)
            }
        };

        let Some(method) = method else {
            self.error(node_id, span, missing_member_error);
            return None;
        };
        if rest.len() > 1 {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::StructPathTooDeep { r#struct: type_name, function: member.clone() },
            );
            return None;
        }
        if !self.check_member_visibility(method.visibility, &owner_module_path, owner_id) {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MethodNotVisible { method: member.clone(), base: r#type.clone() },
            );
            return None;
        }
        if method.fn_type.self_mode.is_some() {
            self.error(
                node_id,
                span,
                AnalysisErrorKind::MemberFunctionWithoutInstance {
                    r#struct: type_name,
                    function: member.clone(),
                },
            );
            return None;
        }

        let fn_type = ResolvedType::Function(method.fn_type);
        let root = CheckedPlaceRoot::Variable {
            decl_id: method.decl_id,
            storage: Storage::Function,
            r#type: fn_type.clone(),
        };
        Some((root, fn_type))
    }

    /// `Enum::Variant` in value position -- the unit construction. Only a
    /// variant with no fields at all -- neither its own body fields nor
    /// (now) the enum's shared dynamic fields -- has one (there is no
    /// implicit zeroing to fill a body with); the result is an ordinary
    /// expression place root whose type statically knows its variant.
    fn resolve_unit_variant(
        &mut self,
        node_id: HirId,
        span: Span,
        cell: &Rc<RefCell<ResolvedEnumType>>,
        variant_index: usize,
        variant: &ResolvedEnumVariant,
        rest: &[Ident],
    ) -> Option<(CheckedPlaceRoot, ResolvedType)> {
        if rest.len() > 1 {
            self.error(node_id, span, AnalysisErrorKind::GenericPathTooDeep { r#type: variant.name.clone() });
            return None;
        }
        let dynamic_field_names: Vec<Ident> = cell.borrow().dynamic_fields.iter().map(|(n, _, _)| n.clone()).collect();
        if !dynamic_field_names.is_empty() || !variant.fields.is_empty() {
            let fields = dynamic_field_names
                .into_iter()
                .chain(variant.fields.iter().map(|(name, _, _)| name.clone()))
                .collect();
            self.error(
                node_id,
                span,
                AnalysisErrorKind::EnumVariantMissingBody {
                    r#enum: cell.borrow().name.clone(),
                    variant: variant.name.clone(),
                    fields,
                },
            );
            return None;
        }
        let r#type = ResolvedType::Enum { cell: cell.clone(), variant: Some(variant_index) };
        let construct = CheckedExprNode {
            id: node_id,
            span,
            r#type: r#type.clone(),
            kind: CheckedExpr::EnumConstruct(CheckedEnumConstruct { variant_index, fields: vec![] }),
        };
        Some((CheckedPlaceRoot::Expr(Box::new(construct)), r#type))
    }
}
