use crate::checked::Storage;
use crate::error::TypeResolutionError;
use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use crate::resolver::{ImportTarget, ItemNamespace, ModuleResolver, ResolveError, ResolvedItem};
use crate::similarity::best_match;
use indexmap::IndexMap;
use omega_hir::HirId;
use omega_parser::prelude::*;

/// What a name resolves to within a scope: the declaring node's own id (so
/// codegen can key its storage maps by declaration identity instead of by
/// name), where its value physically lives, and its resolved type. Anything
/// callable by name is bound here too, with `storage: Storage::Function`;
/// there is no separate function-only table.
#[derive(Debug, Clone)]
pub struct VarBinding {
    pub decl_id: HirId,
    pub storage: Storage,
    pub r#type: ResolvedType,
    /// Where the binding was introduced -- so a later `Redeclaration` error
    /// can point back at it ("first declared here").
    pub span: Span,
    /// `true` only for the shadow binding a matched `match` arm declares to
    /// narrow its scrutinee -- `false` for every ordinary declaration,
    /// including one whose own inferred type happens to be a refined enum
    /// variant. Controls whether `&binding` may keep a refined pointee
    /// type: a `:=`-inferred refined type is a permanent fact about the
    /// binding (see `ResolvedType::accepts`), so a pointer to it staying
    /// refined is sound; a match-narrowed shadow's refinement is only true
    /// for that one arm's lexical duration, so a pointer taken inside it
    /// must still widen.
    pub narrowed: bool,
    /// Whether this binding may be reassigned -- `true` only for a
    /// declaration explicitly written `mut`. Every other binding
    /// (parameters including `self`, struct/enum fields, an un-`mut`
    /// local/global) is `false`; only `self`'s own pointee mutability
    /// varies (`mut self` vs `self`), unrelated to this field.
    pub mutable: bool,
    /// Whether this binding has been read at least once since declaration
    /// -- live-tracked via `Context::mark_used`. Checked at scope-exit for
    /// `AnalysisWarningKind::UnusedVariable`/`UnusedParameter`.
    pub used: bool,
    /// Whether this binding has actually been reassigned since declaration
    /// -- live-tracked via `Context::mark_written`, deliberately
    /// independent of `used` (a write is not itself treated as a read).
    /// Only meaningful when `mutable` is also `true`; checked at scope-exit
    /// for `AnalysisWarningKind::UnnecessaryMut`.
    pub written: bool,
}

#[derive(Debug, Clone)]
pub struct ScopeContext {
    /// `IndexMap`, not `HashMap` -- `warn_unused_bindings` walks every
    /// declared binding at scope-exit and needs deterministic (declaration)
    /// order, which insertion order already gives for free.
    pub declared_variables: IndexMap<(Ident, Origin), VarBinding>,
    /// `IndexMap`, not `HashMap` -- `similar_type_name` picks the first
    /// candidate on an edit-distance tie, and a `HashMap` would make that
    /// pick vary build-to-build for byte-identical source.
    pub defined_types: IndexMap<Ident, ResolvedType>,
}

impl ScopeContext {
    fn new() -> Self {
        Self {
            declared_variables: IndexMap::new(),
            defined_types: IndexMap::new(),
        }
    }

    /// Binds `ident` in this scope, or returns it back as `Err` (with the
    /// existing binding's span, for "first declared here") if already
    /// declared *in this scope*; shadowing an outer scope stays allowed.
    pub fn declare(
        &mut self,
        ident: Ident,
        origin: Origin,
        binding: VarBinding,
    ) -> Result<(), (Ident, Span)> {
        if let Some(existing) = self.declared_variables.get(&(ident.clone(), origin)) {
            return Err((ident, existing.span));
        }
        self.declared_variables.insert((ident, origin), binding);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    scopes: Vec<ScopeContext>,
    /// Every `comp` binding's already-evaluated value, keyed by `decl_id`
    /// rather than kept per-scope -- `decl_id` is already globally unique,
    /// so this needs no shadowing/scope-exit logic of its own.
    comp_values: std::collections::HashMap<HirId, crate::resolved_type::ConstValue>,
}

impl Context {
    pub fn new() -> Self {
        let mut global_scope = ScopeContext::new();
        global_scope.defined_types.extend([
            (Ident("void".into()), ResolvedType::Void),
            (Ident("never".into()), ResolvedType::Never),
            (Ident("bool".into()), ResolvedType::Bool),
            (Ident("char".into()), ResolvedType::Char),
            (Ident("i8".into()), ResolvedType::I8),
            (Ident("i16".into()), ResolvedType::I16),
            (Ident("i32".into()), ResolvedType::I32),
            (Ident("i64".into()), ResolvedType::I64),
            (Ident("isize".into()), ResolvedType::ISize),
            (Ident("u8".into()), ResolvedType::U8),
            (Ident("u16".into()), ResolvedType::U16),
            (Ident("u32".into()), ResolvedType::U32),
            (Ident("u64".into()), ResolvedType::U64),
            (Ident("usize".into()), ResolvedType::USize),
            (Ident("f32".into()), ResolvedType::F32),
            (Ident("f64".into()), ResolvedType::F64),
        ]);
        Self { scopes: vec![global_scope], comp_values: std::collections::HashMap::new() }
    }

    /// Records `decl_id`'s already-evaluated `comp` value -- called once,
    /// by `Analyzer::declare_comp_binding`, alongside the ordinary
    /// `declare_binding` that gives it its `Storage::Comp` place.
    pub fn set_comp_value(&mut self, decl_id: HirId, value: crate::resolved_type::ConstValue) {
        self.comp_values.insert(decl_id, value);
    }

    /// `decl_id`'s recorded `comp` value -- always `Some` for a place whose
    /// root resolved to `Storage::Comp` (the two are only ever produced
    /// together).
    pub fn comp_value(&self, decl_id: HirId) -> Option<&crate::resolved_type::ConstValue> {
        self.comp_values.get(&decl_id)
    }

    // Finder functions
    pub fn find_variable(&self, ident: &Ident, origin: Origin) -> Option<&VarBinding> {
        let found = self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.declared_variables.get(&(ident.clone(), origin)));
        // `self` is an implicit receiver binding synthesized by lowering,
        // not a lexical declaration token. Like type-side `Self`, it must
        // remain available inside a macro-generated method body.
        found.or_else(|| {
            (ident.as_ref() == "self").then(|| {
                self.scopes
                    .iter()
                    .rev()
                    .find_map(|scope| scope.declared_variables.get(&(ident.clone(), Origin::default())))
            })?
        })
    }

    /// Widens `ident`'s *currently visible* binding's type in place the
    /// instant a mutable reference to it is taken (`&mut`, or the auto-ref
    /// for a `mut self` call), rather than shadowing a new one: a writable
    /// alias to the storage now exists, so a later read can no longer trust
    /// a narrower type. See `ResolvedType::accepts` for why this -- rather
    /// than letting a mutable pointer/slice widen implicitly -- is how this
    /// compiler closes that aliasing hole.
    pub fn widen_variable(&mut self, ident: &Ident, origin: Origin) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.declared_variables.get_mut(&(ident.clone(), origin)) {
                binding.r#type = binding.r#type.widened();
                return;
            }
        }
        if ident.as_ref() == "self" {
            for scope in self.scopes.iter_mut().rev() {
                if let Some(binding) = scope
                    .declared_variables
                    .get_mut(&(ident.clone(), Origin::default()))
                {
                    binding.r#type = binding.r#type.widened();
                    return;
                }
            }
        }
    }

    /// Marks the binding identified by `decl_id` as having been read at
    /// least once -- by `decl_id` rather than name, since keying by name
    /// could hit the wrong binding if a same-named shadow was declared in
    /// between resolution and marking. A no-op if `decl_id` doesn't belong
    /// to any live scope (e.g. a field/global, not tracked this way).
    pub fn mark_used(&mut self, decl_id: HirId) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.declared_variables.values_mut().find(|b| b.decl_id == decl_id) {
                binding.used = true;
                return;
            }
        }
    }

    /// Same shape as `mark_used`, for "this binding was actually
    /// reassigned" -- a write-only binding (reassigned but never read
    /// back) still reports `UnusedVariable`, while correctly not also
    /// reporting `UnnecessaryMut`, since `mut` genuinely was exercised.
    pub fn mark_written(&mut self, decl_id: HirId) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.declared_variables.values_mut().find(|b| b.decl_id == decl_id) {
                binding.written = true;
                return;
            }
        }
    }

    pub fn find_defined_type(&self, name: &Ident) -> Option<&ResolvedType> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.defined_types.get(name))
    }

    /// The visible value name (this scope chain only -- an import alias
    /// isn't known here anymore, see `Analyzer::similar_import_alias`)
    /// most similar to `target` -- the "did you mean" candidate for an
    /// undefined-variable diagnostic.
    pub fn similar_variable_name(&self, target: &Ident) -> Option<Ident> {
        best_match(
            target,
            self.scopes
                .iter()
                .flat_map(|scope| scope.declared_variables.keys().map(|(ident, _)| ident)),
        )
    }

    /// The visible type name most similar to `target` -- builtins and
    /// locally defined types only (see `similar_variable_name`).
    pub fn similar_type_name(&self, target: &Ident) -> Option<Ident> {
        best_match(target, self.scopes.iter().flat_map(|scope| scope.defined_types.keys()))
    }

    /// A function/method signature's param and return types are never
    /// embedded inline into anything's layout -- always `indirect = true`,
    /// regardless of what the caller itself was.
    pub fn resolve_function_type(
        &self,
        fntype: FunctionType,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        bypass: bool,
    ) -> Result<ResolvedFunctionType, TypeResolutionError> {
        let params = fntype
            .params
            .into_iter()
            .map(|param| {
                self.resolve_type(param.r#type, resolver, module_path, true, bypass)
                    .map(|resolved| (param.ident, resolved))
            })
            .collect::<Result<Vec<(Ident, ResolvedType)>, TypeResolutionError>>()?;
        Ok(ResolvedFunctionType {
            params,
            return_type: Box::new(self.resolve_type(*fntype.return_type, resolver, module_path, true, bypass)?),
            is_variadic: fntype.is_variadic,
            self_mode: fntype.self_mode,
        })
    }

    /// Resolves `path` to an absolute `[module_path.., name]`, the shared
    /// logic behind `Type::Named`'s and `Type::Generic`'s unqualified/
    /// qualified branches: for an unqualified `path`, an import alias
    /// resolving to a *generic* item wins over the implicit
    /// own-module-prefixed fallback. For a qualified `path`, its head must
    /// resolve to a *module* alias; the rest is appended onto its absolute
    /// path.
    ///
    /// `pub(crate)` so `Analyzer::resolve_spec_dependencies` can resolve
    /// *which* spec a raw dependency reference names without resolving its
    /// type arguments too.
    pub(crate) fn resolve_absolute_item_path(
        &self,
        resolver: &mut dyn ModuleResolver,
        path: &Path,
        module_path: &[Ident],
    ) -> Result<Vec<Ident>, TypeResolutionError> {
        let resolution_module = resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| module_path.to_vec());
        if path.is_unqualified() {
            // `ImportTarget::Item`'s eagerly-resolved snapshot is ignored
            // here -- this function's only job is the absolute path; every
            // caller re-resolves through `resolver` with its own real
            // `indirect`/args, never trusting a cached snapshot.
            match resolver
                .resolve_import_alias(&resolution_module, &path.head)
                .map_err(TypeResolutionError::ModuleResolution)?
            {
                Some(ImportTarget::GenericItem(absolute)) => return Ok(absolute),
                Some(ImportTarget::Item(absolute, _)) => return Ok(absolute),
                _ => {}
            }
            Ok(resolution_module
                .iter()
                .cloned()
                .chain(std::iter::once(path.head.clone()))
                .collect())
        } else {
            match resolver
                .resolve_import_alias(&resolution_module, &path.head)
                .map_err(TypeResolutionError::ModuleResolution)?
            {
                Some(ImportTarget::Module(target)) => Ok(target.into_iter().chain(path.tail.iter().cloned()).collect()),
                _ => Err(TypeResolutionError::ModuleNotImported {
                    name: path.head.clone(),
                    similar: best_match(
                        &path.head,
                        resolver.import_alias_names(&resolution_module).iter(),
                    ),
                }),
            }
        }
    }

    /// Resolves one written type to its concrete form. `module_path` is
    /// the caller's own absolute module path, used to build an implicit
    /// absolute path for an unqualified reference that isn't a builtin or
    /// a local binding.
    ///
    /// `indirect` says whether `typ` sits somewhere that never embeds its
    /// referent inline into another type's layout -- the distinction that
    /// lets a self-referential pointer field resolve while its own type is
    /// still being collected, and that rejects a by-value cycle. It starts
    /// as whatever the caller passed and only ever turns on as the walk
    /// descends: `Pointer`/`Array` and a `Function`'s param/return types
    /// are never embedded inline, so everything beneath them is indirect
    /// regardless of what it started as; `SizedArray` carries its element
    /// inline, so it passes the current value through unchanged. `bypass`
    /// is the `reveal` modifier.
    pub fn resolve_type(
        &self,
        typ: Type,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        match typ {
            Type::Named(path) => self.resolve_named_type(path, resolver, module_path, indirect, bypass),
            Type::Generic(path, args) => self.resolve_generic_type(path, args, resolver, module_path, indirect, bypass),
            Type::Pointer(pointee, mutable) => {
                self.resolve_pointer_type(*pointee, mutable, resolver, module_path, bypass)
            }
            Type::SpecObject(pointee, mutable) => {
                self.resolve_spec_object_type(*pointee, mutable, resolver, module_path, bypass)
            }
            // A bare `spec Foo` reaching ordinary type resolution at all
            // means it's sitting somewhere this sugar was never defined for
            // -- see `Type::SpecStatic`'s doc comment. The two legitimate
            // positions (a parameter type, and a return type inside a
            // spec's own function declaration) are both intercepted *before*
            // `resolve_type` is ever called on this shape (HIR-lowering
            // desugaring for the former, `resolve_raw_spec_fn_type`/
            // `is_object_safe` for the latter).
            Type::SpecStatic(pointee) => {
                let name = match pointee.as_ref() {
                    Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
                    _ => Ident("<spec>".to_string()),
                };
                Err(TypeResolutionError::SpecStaticNotAllowedHere(name))
            }
            Type::Function(fntyp) => {
                Ok(ResolvedType::Function(self.resolve_function_type(fntyp, resolver, module_path, bypass)?))
            }
            // Always invalid reached directly -- their only legal uses
            // (behind a leading `*`, or -- for `InferredArray` only -- as a
            // declaration's own type annotation with an inferred length)
            // are both intercepted *before* they ever reach this dispatch;
            // see `resolve_pointer_type` and `Analyzer::
            // resolve_typed_decl_init` respectively.
            Type::InferredArray(_) => Err(TypeResolutionError::BareUnsizedArray),
            Type::UnknownSizeArray(_) => Err(TypeResolutionError::BareUnknownSizeArray),
            Type::SizedArray(item, size) => {
                let size = size.parse::<u32>().map_err(|_| TypeResolutionError::InvalidArraySize(size.clone()))?;
                let item = self.resolve_type(*item, resolver, module_path, indirect, bypass)?;
                Ok(ResolvedType::SizedArray(Box::new(item), size))
            }
        }
    }

    /// A plain named type: an enum variant, a locally defined type (a
    /// generic parameter or a builtin), an import alias, or -- failing all
    /// of those -- an item in this module.
    fn resolve_named_type(
        &self,
        path: Path,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolution_module = resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| module_path.to_vec());
        let resolved = {
            if let Some(resolved) = self.try_resolve_enum_variant_type(&path, resolver, module_path, indirect, bypass)? {
                resolved
            } else if path.is_unqualified() {
                if let Some(local) = self.find_defined_type(&path.head) {
                    local.to_owned()
                } else {
                    // An import alias, lazily resolved: find the
                    // absolute path the alias names, then resolve *that*
                    // through `resolve_item` with this reference's own
                    // `indirect` -- deliberately never trusting
                    // `ImportTarget::Item`'s eagerly-resolved snapshot,
                    // which was always produced with `indirect = true`
                    // (see findings for the cycle-detection bug this
                    // used to cause). Re-running `resolve_item` costs
                    // nothing extra once the item is already `Done`.
                    let alias = resolver
                        .resolve_import_alias(&resolution_module, &path.head)
                        .map_err(TypeResolutionError::ModuleResolution)?;
                    if let Some(ImportTarget::Item(_, ResolvedItem::Value { .. })) = alias {
                        return Err(TypeResolutionError::NotAType(vec![path.head.clone()]));
                    }
                    let absolute = match alias {
                        Some(ImportTarget::Item(absolute, _))
                        | Some(ImportTarget::GenericItem(absolute))
                        | Some(ImportTarget::Module(absolute)) => absolute,
                        None => resolution_module
                            .iter()
                            .cloned()
                            .chain(std::iter::once(path.head.clone()))
                            .collect(),
                    };
                    match resolver.resolve_item(&resolution_module, &absolute, &[], indirect, bypass) {
                        Ok(ResolvedItem::Type(t)) => t,
                        Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => return Err(TypeResolutionError::NotAType(absolute)),
                        // A bare type name that doesn't exist gets one
                        // more try against every exposed name in `core`'s
                        // ambient tree before giving up with a typo
                        // suggestion -- mirrors `resolve_generic_type`'s
                        // identical fallback, needed here too since a bare
                        // named type never reaches that function.
                        Err(ResolveError::UnknownItem { .. }) => {
                            match resolver.ambient_core_candidates(&resolution_module, &path.head) {
                                Ok(Some(ambient_absolute)) => {
                                    match resolver.resolve_item(&resolution_module, &ambient_absolute, &[], indirect, bypass) {
                                        Ok(ResolvedItem::Type(t)) => t,
                                        Ok(ResolvedItem::Value { .. }) | Ok(ResolvedItem::Gap(_)) => {
                                            return Err(TypeResolutionError::NotAType(ambient_absolute));
                                        }
                                        Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                                    }
                                }
                                Ok(None) => {
                                    let similar = self
                                        .similar_type_name(&path.head)
                                        .or_else(|| best_match(&path.head, resolver.import_alias_names(&resolution_module).iter()))
                                        .or_else(|| resolver.similar_item_name(&resolution_module, &path.head, ItemNamespace::Type));
                                    return Err(TypeResolutionError::UnrecognizedNamedType {
                                        name: path.head.clone(),
                                        similar,
                                    });
                                }
                                Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                            }
                        }
                        Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                    }
                }
            } else {
                // A qualified type reference (`mymodule::Foo`) -- `path`'s
                // head must already be an imported module alias.
                let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
                match resolver
                    .resolve_item(&resolution_module, &absolute, &[], indirect, bypass)
                    .map_err(TypeResolutionError::ModuleResolution)?
                {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } | ResolvedItem::Gap(_) => return Err(TypeResolutionError::NotAType(absolute)),
                }
            }
        
        };
        Ok(resolved)
    }

    /// `Path<Type, ...>` -- a generic item referenced with explicit type
    /// arguments (e.g. `List<u32>`).
    fn resolve_generic_type(
        &self,
        path: Path,
        args: Vec<Type>,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolution_module = resolver
            .macro_origin_module(path.origin)
            .unwrap_or_else(|| module_path.to_vec());
        let resolved = {
            let resolved_args = args
                .into_iter()
                .map(|arg| self.resolve_type(arg, resolver, module_path, true, bypass))
                .collect::<Result<Vec<_>, _>>()?;
            let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
            let result = resolver.resolve_item(&resolution_module, &absolute, &resolved_args, indirect, bypass);
            // An unqualified name that doesn't resolve to anything local
            // gets one more try against every exposed name in `core`'s
            // ambient tree (a full prelude, not the short hardcoded table
            // this used to be).
            let result = match (&result, path.is_unqualified()) {
                (Err(ResolveError::UnknownItem { .. }), true) => {
                    match resolver.ambient_core_candidates(&resolution_module, &path.head) {
                        Ok(Some(ambient_absolute)) => {
                            resolver.resolve_item(&resolution_module, &ambient_absolute, &resolved_args, indirect, bypass)
                        }
                        Ok(None) => result,
                        Err(e) => Err(e),
                    }
                }
                _ => result,
            };
            match result.map_err(TypeResolutionError::ModuleResolution)? {
                ResolvedItem::Type(t) => t,
                ResolvedItem::Value { .. } | ResolvedItem::Gap(_) => return Err(TypeResolutionError::NotAType(absolute)),
            }

        };
        Ok(resolved)
    }

    /// `*T`, which is not always a thin pointer. Dispatches directly on
    /// `pointee_type`'s own raw shape: `*[]T`/`*[?]T` are caught before any
    /// generic resolution happens; every other pointee (including `*[N]T`)
    /// falls through to ordinary resolution.
    fn resolve_pointer_type(
        &self,
        pointee_type: Type,
        mutable: bool,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        bypass: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        match pointee_type {
            Type::InferredArray(item) => {
                let item = self.resolve_type(*item, resolver, module_path, true, bypass)?;
                Ok(ResolvedType::Slice { item: Box::new(item), mutable })
            }
            Type::UnknownSizeArray(item) => {
                let item = self.resolve_type(*item, resolver, module_path, true, bypass)?;
                Ok(ResolvedType::Array(Box::new(item), mutable))
            }
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "str" => {
                Ok(ResolvedType::Str { mutable })
            }
            // `*self`/`*mut self` always lowers to exactly this raw
            // shape: when a `primitive str`/`primitive<T> []T` method's
            // `Self` is substituted with `Str`/`Array`, this re-stamps
            // rather than double-wraps, so `*self` comes out as the real
            // `Str`/`Slice` receiver instead of a pointer to one.
            // Deliberately **not** applied to an ordinary generic
            // parameter (`T` in `out: *mut T`) that might resolve to
            // `Str`/`Array` through unrelated substitution -- see findings
            // for the confirmed bug this distinction prevents.
            Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "Self" => {
                match self.resolve_named_type(path, resolver, module_path, true, bypass)? {
                    ResolvedType::Str { .. } => Ok(ResolvedType::Str { mutable }),
                    ResolvedType::Array(item, _) => Ok(ResolvedType::Slice { item, mutable }),
                    // `Self` already *being* the slice is the shape a
                    // `conform []u8 to Spec` binds, as opposed to the
                    // `Array` stand-in a `primitive<T> [?]T` block
                    // substitutes -- see findings for the bug this arm
                    // fixes.
                    ResolvedType::Slice { item, .. } => Ok(ResolvedType::Slice { item, mutable }),
                    resolved => Ok(ResolvedType::Pointer { pointee: Box::new(resolved), mutable }),
                }
            }
            other => {
                let resolved = self.resolve_type(other, resolver, module_path, true, bypass)?;
                Ok(ResolvedType::Pointer { pointee: Box::new(resolved), mutable })
            }
        }
    }

    /// `spec *Animal`/`spec *mut Animal` -- a dynamic-dispatch spec-object
    /// pointer. Never `indirect`-sensitive itself: a spec object is always a
    /// fat pointer, never embedded inline.
    fn resolve_spec_object_type(
        &self,
        pointee: Type,
        mutable: bool,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        bypass: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let pointee = Box::new(pointee);
        let resolved = {
            let type_args = match pointee.as_ref() {
                Type::Generic(_, args) => args.clone(),
                _ => vec![],
            };
            let resolved_args = type_args
                .into_iter()
                .map(|a| self.resolve_type(a, resolver, module_path, true, bypass))
                .collect::<Result<Vec<_>, _>>()?;
            let pointee_name = match pointee.as_ref() {
                Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
                _ => Ident("<spec>".to_string()),
            };
            match self.resolve_type(*pointee, resolver, module_path, true, bypass)? {
                ResolvedType::Spec(spec) => {
                    if !spec.borrow().is_object_safe {
                        return Err(TypeResolutionError::SpecNotObjectSafe(pointee_name));
                    }
                    ResolvedType::SpecObject { spec, type_args: resolved_args, mutable }
                }
                _ => return Err(TypeResolutionError::NotASpec(pointee_name)),
            }
        
        };
        Ok(resolved)
    }

    /// If `path`'s last segment names a variant of the enum its remaining
    /// segments resolve to (`Entity::Person`), resolves to that variant's
    /// own refined type -- the type-position mirror of
    /// `Analyzer::resolve_type_member`, letting a variant be named directly
    /// in a type annotation (`x: *Entity::Person`). `Ok(None)`, not an
    /// error, whenever `path` has one segment or its prefix isn't a plain
    /// enum, so the caller falls through to ordinary handling; `Err` only
    /// once the prefix genuinely is a plain enum but the last segment
    /// isn't one of its variants.
    fn try_resolve_enum_variant_type(
        &self,
        path: &Path,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
        bypass: bool,
    ) -> Result<Option<ResolvedType>, TypeResolutionError> {
        let Some((variant_name, prefix_tail)) = path.tail.split_last() else { return Ok(None) };
        let prefix = Type::Named(Path {
            head: path.head.clone(),
            tail: prefix_tail.to_vec(),
            origin: path.origin,
        });
        let Ok(ResolvedType::Enum { cell, variant: None }) = self.resolve_type(prefix, resolver, module_path, indirect, bypass) else {
            return Ok(None);
        };
        let found = cell.borrow().variant(variant_name).map(|(idx, _)| idx);
        match found {
            Some(idx) => Ok(Some(ResolvedType::Enum { cell: cell.clone(), variant: Some(idx) })),
            None => {
                let similar = best_match(variant_name, cell.borrow().variants.iter().map(|v| &v.name));
                Err(TypeResolutionError::NoSuchVariantForType {
                    r#enum: cell.borrow().name.clone(),
                    name: variant_name.clone(),
                    similar,
                })
            }
        }
    }

    // Scope helpers
    pub fn current_scope(&mut self) -> &mut ScopeContext {
        self.scopes.last_mut().unwrap()
    }

    pub fn enter_scope(&mut self) -> &mut ScopeContext {
        self.scopes.push(ScopeContext::new());
        self.current_scope()
    }

    pub fn leave_scope(&mut self) -> ScopeContext {
        if self.scopes.len() == 1 {
            // The Context must always have at least one scope.
            let scope = self.scopes.remove(0);
            self.scopes.push(ScopeContext::new());
            return scope;
        }

        self.scopes
            .pop()
            .expect("BAD: Context does not have a scope. This should NEVER happen.")
    }
}
