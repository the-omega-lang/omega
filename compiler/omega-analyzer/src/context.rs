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
/// callable by name -- extern function decls, local function defs, struct
/// methods within their own struct scope -- is bound here too, with
/// `storage: Storage::Function`; there is no separate function-only table.
#[derive(Debug, Clone)]
pub struct VarBinding {
    pub decl_id: HirId,
    pub storage: Storage,
    pub r#type: ResolvedType,
    /// Where the binding was introduced -- so a later `Redeclaration` error
    /// can point back at it ("first declared here").
    pub span: Span,
    /// `true` only for the shadow binding a matched `match` arm declares to
    /// narrow its scrutinee (`Analyzer::analyze_enum_match`) -- `false` for
    /// every ordinary declaration, including one whose own inferred type
    /// happens to be a refined enum variant (`a := Entity::Person { ... }`).
    /// The distinction matters for exactly one thing: whether `&binding`
    /// may keep a refined pointee type. A `:=`-inferred refined type is a
    /// *permanent* fact about the binding (assigning a different variant to
    /// it later would already be rejected by `ResolvedType::accepts`), so a
    /// pointer to it staying refined is sound; a match-narrowed shadow's
    /// refinement is only true for the lexical duration of that one arm --
    /// the underlying storage can still hold a different variant once the
    /// arm ends, so a pointer taken inside it must still widen, exactly
    /// like before this field existed. See `Analyzer`'s `HirExpr::AddressOf`
    /// arm.
    pub narrowed: bool,
    /// Whether this binding may be reassigned (`x = ...`/`++x`/`--x`) --
    /// `true` only for a declaration explicitly written `mut` (see
    /// `DeclarationStmt`/`WalrusStmt`'s own `mutable` fields). Every other
    /// binding -- parameters (including `self`), struct/enum fields, and an
    /// un-`mut` local/global -- is `false`; only `self`'s own *pointee*
    /// mutability varies (`mut self` vs `self`, a `ResolvedType::Pointer`
    /// concern, unrelated to this field). See `Analyzer::analyze_place`'s
    /// doc comment for how this feeds into a whole place's mutability.
    pub mutable: bool,
    /// Whether this binding has been read at least once since declaration
    /// -- live-tracked (not a post-hoc tree walk, since `mutable` never
    /// survives onto the checked tree at all -- see `mark_written`'s doc
    /// comment) via `Context::mark_used`, called from the one place an
    /// ordinary read of a place actually happens (`Analyzer::analyze_expr`'s
    /// `HirExpr::Place` arm). Checked at scope-exit for
    /// `AnalysisWarningKind::UnusedVariable`/`UnusedParameter` -- see
    /// `Analyzer::warn_unused_bindings`.
    pub used: bool,
    /// Whether this binding has actually been reassigned (`=`, a compound
    /// assignment, `++`/`--`, or `&mut`) since declaration -- live-tracked
    /// via `Context::mark_written`, called from `Analyzer::
    /// require_mutable_place`, the one existing choke point for "this place
    /// is about to be written through." Only ever meaningful when `mutable`
    /// is also `true` (an un-`mut` binding can never reach
    /// `require_mutable_place` successfully in the first place); checked at
    /// scope-exit for `AnalysisWarningKind::UnnecessaryMut`.
    pub written: bool,
}

#[derive(Debug, Clone)]
pub struct ScopeContext {
    /// `IndexMap`, not `HashMap` -- `Analyzer::warn_unused_bindings` walks
    /// every declared binding at scope-exit to report `UnusedVariable`/
    /// `UnusedParameter`/`UnnecessaryMut`, and needs that walk to visit
    /// bindings in a deterministic (declaration) order rather than
    /// `HashMap`'s per-process-random one -- insertion order already *is*
    /// declaration order here, so this gets that for free.
    pub declared_variables: IndexMap<Ident, VarBinding>,
    /// `IndexMap`, not `HashMap` -- `Context::similar_type_name` (the "did
    /// you mean" candidate source for an unrecognized type name) iterates
    /// every scope's `defined_types` and picks the first candidate on an
    /// edit-distance tie (`similarity::best_match`'s own `min_by_key`
    /// semantics) -- with a `HashMap`, which candidate wins a tie varied
    /// build-to-build for byte-identical source, an `IndexMap` makes that
    /// deterministic (declaration order) instead.
    pub defined_types: IndexMap<Ident, ResolvedType>,
}

impl ScopeContext {
    fn new() -> Self {
        Self {
            declared_variables: IndexMap::new(),
            defined_types: IndexMap::new(),
        }
    }

    /// Binds `ident` in this scope, or returns it back as `Err` -- together
    /// with the existing binding's span, for the "first declared here"
    /// label -- if it's already declared *in this scope*; shadowing an
    /// outer scope is ordinary lexical scoping and stays allowed.
    /// Centralizes a check that used to live, wrongly, in codegen (a
    /// name-keyed stack-slot map, which only coincidentally caught
    /// same-function redeclaration and never caught it for parameters at
    /// all).
    pub fn declare(&mut self, ident: Ident, binding: VarBinding) -> Result<(), (Ident, Span)> {
        if let Some(existing) = self.declared_variables.get(&ident) {
            return Err((ident, existing.span));
        }
        self.declared_variables.insert(ident, binding);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    scopes: Vec<ScopeContext>,
}

impl Context {
    pub fn new() -> Self {
        let mut global_scope = ScopeContext::new();
        global_scope.defined_types.extend([
            // Standard types
            (Ident("void".into()), ResolvedType::Void),
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
        Self { scopes: vec![global_scope] }
    }

    // Finder functions
    pub fn find_variable(&self, ident: &Ident) -> Option<&VarBinding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.declared_variables.get(ident))
    }

    /// "De-assumes" a proof the instant a mutable reference to `ident`'s
    /// current place is taken (`&mut`, or the auto-ref for a `mut self`
    /// method call) -- widens its *currently visible* binding's type in
    /// place, wherever it's found (innermost scope first, matching
    /// `find_variable`'s own walk), rather than shadowing a new one: a
    /// writable alias to the storage now exists, so any later direct read
    /// of `ident` within the same (or an enclosing) scope can no longer
    /// trust a narrower type than the plain one. See
    /// `ResolvedType::accepts`'s doc comment for why this -- rather than
    /// ever letting a *mutable* pointer/slice widen implicitly -- is how
    /// this compiler closes that aliasing hole.
    pub fn widen_variable(&mut self, ident: &Ident) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.declared_variables.get_mut(ident) {
                binding.r#type = binding.r#type.widened();
                return;
            }
        }
    }

    /// Marks the binding identified by `decl_id` as having been read at
    /// least once -- scans live scopes innermost-first (same walk as
    /// `widen_variable`), but by `decl_id` rather than name: the caller only
    /// ever has a resolved `CheckedPlace`'s `decl_id` by the time it can
    /// call this, and keying by name could hit the wrong binding if a
    /// same-named shadow was declared in between resolution and marking.
    /// A no-op if `decl_id` doesn't belong to any live scope (e.g. it names
    /// a field/global, which aren't tracked this way at all).
    pub fn mark_used(&mut self, decl_id: HirId) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.declared_variables.values_mut().find(|b| b.decl_id == decl_id) {
                binding.used = true;
                return;
            }
        }
    }

    /// Same shape as `mark_used`, for "this binding was actually
    /// reassigned" -- deliberately independent of `used` (a write is *not*
    /// itself treated as a read): a write-only binding (reassigned but
    /// never read back) still reports `UnusedVariable` -- it matches that
    /// warning's exact definition, "never read" -- while correctly *not*
    /// also reporting `UnnecessaryMut`, since `mut` genuinely was exercised
    /// here (see `Analyzer::warn_unused_bindings`'s `used &&`-gated check,
    /// which relies on this independence).
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
    /// isn't known here at all anymore, see `Analyzer::similar_import_alias`)
    /// most similar to `target`, if any is close enough -- the "did you
    /// mean" candidate for an undefined-variable diagnostic.
    pub fn similar_variable_name(&self, target: &Ident) -> Option<Ident> {
        best_match(target, self.scopes.iter().flat_map(|scope| scope.declared_variables.keys()))
    }

    /// The visible type name most similar to `target` -- builtins and
    /// locally defined types only (see `similar_variable_name`'s doc
    /// comment on why import aliases aren't known here anymore).
    pub fn similar_type_name(&self, target: &Ident) -> Option<Ident> {
        best_match(target, self.scopes.iter().flat_map(|scope| scope.defined_types.keys()))
    }

    /// A function/method signature's param and return types are never
    /// embedded inline into anything's layout (a function is called, not
    /// laid out inline) -- always `indirect = true`, regardless of what the
    /// caller itself was.
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
            .map(|(ident, typ)| {
                self.resolve_type(typ, resolver, module_path, true, bypass)
                    .map(|resolved| (ident, resolved))
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
    /// qualified branches (kept as one method so this priority order is only
    /// written once): for an unqualified `path`, an import alias resolving
    /// to a *generic* item wins over the implicit own-module-prefixed
    /// fallback -- a generic item is never itself a `find_defined_type`
    /// entry, so callers still check that first, separately, for ordinary
    /// local shadowing. For a qualified `path`, `path`'s head must resolve
    /// to a *module* alias; the rest is appended onto its absolute path.
    ///
    /// `pub(crate)` (not just used internally by `resolve_type`) so
    /// `Analyzer::resolve_spec_dependencies` can resolve *which* spec a raw
    /// dependency reference names without resolving its type arguments too
    /// -- see that function's own doc comment for why the two need to be
    /// separable there specifically.
    pub(crate) fn resolve_absolute_item_path(
        &self,
        resolver: &mut dyn ModuleResolver,
        path: &Path,
        module_path: &[Ident],
    ) -> Result<Vec<Ident>, TypeResolutionError> {
        if path.is_unqualified() {
            if let Some(ImportTarget::GenericItem(absolute)) =
                resolver.resolve_import_alias(module_path, &path.head).map_err(TypeResolutionError::ModuleResolution)?
            {
                return Ok(absolute);
            }
            Ok(module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect())
        } else {
            match resolver.resolve_import_alias(module_path, &path.head).map_err(TypeResolutionError::ModuleResolution)? {
                Some(ImportTarget::Module(target)) => Ok(target.into_iter().chain(path.tail.iter().cloned()).collect()),
                _ => Err(TypeResolutionError::ModuleNotImported {
                    name: path.head.clone(),
                    similar: best_match(&path.head, resolver.import_alias_names(module_path).iter()),
                }),
            }
        }
    }

    /// `module_path` is the *caller's own* absolute module path -- used to
    /// build an implicit absolute path for an unqualified reference that
    /// isn't a builtin or a local (function-body-level) binding, so it can
    /// be resolved the exact same way a qualified cross-module one is (see
    /// `ModuleResolver::resolve_item`'s doc comment: there's no longer an
    /// architectural difference between the two).
    ///
    /// `indirect` is true whenever `typ` itself sits somewhere that never
    /// embeds its referent inline into another type's layout. It starts out
    /// as whatever the caller passed and only ever *turns on* as the walk
    /// descends: `Pointer`/`Array` (a thin pointer) and a `Function`'s own
    /// param/return types are never embedded inline into anything, so
    /// everything beneath them is indirect regardless of what it started as;
    /// `SizedArray` carries its element inline, so it just passes the
    /// current value through unchanged. See `ModuleResolver::resolve_item`
    /// for what this distinction ultimately protects.
    /// Resolves one written type to its concrete form.
    ///
    /// `indirect` says whether this reference sits somewhere that never
    /// embeds its referent inline (behind a pointer, or in a function
    /// signature) -- the distinction that lets a self-referential pointer
    /// field resolve while its own type is still being collected, and that
    /// rejects a by-value cycle. `bypass` is the `reveal` modifier.
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
            // positions (a parameter type, a function's own return type)
            // are both intercepted *before* `resolve_type` is ever called
            // on this shape (HIR-lowering desugaring for the former,
            // `resolve_raw_spec_fn_type`/the driver's spec-return inference
            // for the latter).
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
            Type::Array(item) => {
                Ok(ResolvedType::Array(Box::new(self.resolve_type(*item, resolver, module_path, true, bypass)?)))
            }
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
        let resolved = {
            if let Some(resolved) = self.try_resolve_enum_variant_type(&path, resolver, module_path, indirect, bypass)? {
                resolved
            } else if path.is_unqualified() {
                if let Some(local) = self.find_defined_type(&path.head) {
                    local.to_owned()
                } else {
                    // An import alias, lazily resolved -- an ordinary,
                    // non-generic *type* alias, a generic item, or a
                    // module alias all end up resolved the same way
                    // here: find the absolute path the alias names, then
                    // resolve *that* through `resolve_item` with *this*
                    // reference's own `indirect`; no alias at all falls
                    // through to the implicit own-module assumption,
                    // exactly as before.
                    //
                    // Deliberately never short-circuits on
                    // `ImportTarget::Item`'s own eagerly-resolved
                    // snapshot -- that snapshot was always produced with
                    // `indirect = true` (see its doc comment), so
                    // trusting it directly here would silently drop this
                    // reference's real `indirect` whenever it's `false`
                    // (a struct/enum/union field's own type, embedded
                    // inline) -- exactly the gap that let a mutual
                    // by-value struct cycle reached through a bare
                    // import alias slip past the cycle check a
                    // module-qualified reference already got. Re-running
                    // `resolve_item` costs nothing extra once the item
                    // is already `Done` (a couple of hashmap lookups,
                    // the same cost `ImportTarget::Item` itself would
                    // have paid to read back its own snapshot); it only
                    // matters when the item is still genuinely
                    // `InProgress`, which is exactly the case this
                    // exists to catch.
                    let alias = resolver
                        .resolve_import_alias(module_path, &path.head)
                        .map_err(TypeResolutionError::ModuleResolution)?;
                    if let Some(ImportTarget::Item(_, ResolvedItem::Value { .. })) = alias {
                        return Err(TypeResolutionError::NotAType(vec![path.head.clone()]));
                    }
                    let absolute = match alias {
                        Some(ImportTarget::Item(absolute, _))
                        | Some(ImportTarget::GenericItem(absolute))
                        | Some(ImportTarget::Module(absolute)) => absolute,
                        None => module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect(),
                    };
                    match resolver.resolve_item(module_path, &absolute, &[], indirect, bypass) {
                        Ok(ResolvedItem::Type(t)) => t,
                        Ok(ResolvedItem::Value { .. }) => return Err(TypeResolutionError::NotAType(absolute)),
                        // The implicit own-module fallback missing isn't
                        // a module problem from the user's point of
                        // view -- they wrote a bare type name that
                        // doesn't exist. Report it as exactly that, with
                        // a typo suggestion where one is close enough --
                        // from the visible scopes, this module's own
                        // import aliases, then its top-level structs
                        // (which only the resolver can enumerate).
                        Err(ResolveError::UnknownItem { .. }) => {
                            let similar = self
                                .similar_type_name(&path.head)
                                .or_else(|| best_match(&path.head, resolver.import_alias_names(module_path).iter()))
                                .or_else(|| resolver.similar_item_name(module_path, &path.head, ItemNamespace::Type));
                            return Err(TypeResolutionError::UnrecognizedNamedType { name: path.head.clone(), similar });
                        }
                        Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                    }
                }
            } else {
                // A qualified type reference (`mymodule::Foo`) -- `path`'s
                // head must already be an imported module alias; the rest
                // is resolved across modules by `resolver`, never locally.
                let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
                match resolver
                    .resolve_item(module_path, &absolute, &[], indirect, bypass)
                    .map_err(TypeResolutionError::ModuleResolution)?
                {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
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
        let resolved = {
            let resolved_args = args
                .into_iter()
                .map(|arg| self.resolve_type(arg, resolver, module_path, true, bypass))
                .collect::<Result<Vec<_>, _>>()?;
            let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
            let result = resolver.resolve_item(module_path, &absolute, &resolved_args, indirect, bypass);
            // An unqualified name that doesn't resolve to anything local
            // gets one more try against `core`, for a short, hardcoded list
            // of well-known generic items the `for`-in-loop feature depends
            // on (`Option`/`Iterator`/`ToIterator`) -- see `ambient_core_path`'s
            // doc comment for why this exists and why it's deliberately not
            // a general prelude/auto-import mechanism.
            let result = match (&result, path.is_unqualified().then(|| ambient_core_path(&path.head)).flatten())
            {
                (Err(ResolveError::UnknownItem { .. }), Some(ambient_absolute)) => {
                    resolver.resolve_item(module_path, &ambient_absolute, &resolved_args, indirect, bypass)
                }
                _ => result,
            };
            match result.map_err(TypeResolutionError::ModuleResolution)? {
                ResolvedItem::Type(t) => t,
                ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
            }

        };
        Ok(resolved)
    }

    /// `*T`, which is not always a thin pointer.
    fn resolve_pointer_type(
        &self,
        pointee_type: Type,
        mutable: bool,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        bypass: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let pointee_type = Box::new(pointee_type);
        let resolved = {
            let is_bare_str = matches!(
                pointee_type.as_ref(),
                Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "str"
            );
            if is_bare_str {
                ResolvedType::Str { mutable }
            } else {
                match self.resolve_type(*pointee_type, resolver, module_path, true, bypass)? {
                    ResolvedType::Array(item_type) => ResolvedType::Slice { item: item_type, mutable },
                    // A pointee that resolves (not through the literal
                    // `str` syntax above, but indirectly -- e.g. through
                    // a `for str` extension spec's `Self` substitution,
                    // see `HirSpecDef::target`) to `Str` gets the same
                    // treatment as the literal case: re-stamped with
                    // *this* pointer's own mutability, never
                    // double-wrapped. `Str` (like `Slice`) is already
                    // its own fat-pointer value representation -- a
                    // pointer to one is the same shape, just a
                    // (possibly) different mutability.
                    ResolvedType::Str { .. } => ResolvedType::Str { mutable },
                    other => ResolvedType::Pointer { pointee: Box::new(other), mutable },
                }
            }
        
        };
        Ok(resolved)
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
    /// segments resolve to (`Entity::Person`, or `mymodule::Entity::Person`),
    /// resolves to that variant's own refined type
    /// (`ResolvedType::Enum { variant: Some(_) }`) -- the type-position
    /// mirror of `Analyzer::resolve_type_member`'s identical lookup on the
    /// expression side, letting a variant be named directly in a type
    /// annotation (`x: *Entity::Person`). Returns `Ok(None)` -- not an error
    /// -- whenever `path` has only one segment, or its prefix doesn't
    /// resolve to a plain enum at all, so the caller falls through to
    /// ordinary module-qualified-path handling unchanged; only returns
    /// `Err` once the prefix genuinely *is* a plain enum but the last
    /// segment isn't one of its variants -- a real, actionable mistake.
    fn try_resolve_enum_variant_type(
        &self,
        path: &Path,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
        bypass: bool,
    ) -> Result<Option<ResolvedType>, TypeResolutionError> {
        let Some((variant_name, prefix_tail)) = path.tail.split_last() else { return Ok(None) };
        let prefix = Type::Named(Path { head: path.head.clone(), tail: prefix_tail.to_vec() });
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
            // The Context must always
            // have at least one scope
            let scope = self.scopes.remove(0);
            self.scopes.push(ScopeContext::new());
            return scope;
        }

        self.scopes
            .pop()
            .expect("BAD: Context does not have a scope. This should NEVER happen.")
    }
}

/// The absolute path of a well-known `core` generic item a bare,
/// unqualified name might refer to, with no `import` needed -- `None` for
/// every other name. This is deliberately a short, hardcoded table, not a
/// general prelude/auto-import mechanism: nothing else in this language is
/// ambiently available without an explicit import, and this doesn't
/// change that -- it exists only because `for <binding> in <iterator>`
/// needs `Option`/`Iterator`/`ToIterator` to work the same way in *every*
/// file, the same way `core`'s `for`-attached extension methods are
/// already discovered without requiring an import (`omega_driver::
/// extensions`). A free function (not a `Context` method) since both
/// `Context::resolve_generic_type` (type positions) and `Analyzer::
/// resolve_item_checked_with_ambient_fallback` (expression positions,
/// `analysis/mod.rs`) need it -- consulted only after ordinary
/// local/import resolution of the same name already failed, at each call
/// site, so it can never shadow a user's own same-named type.
pub(crate) fn ambient_core_path(name: &Ident) -> Option<Vec<Ident>> {
    const WELL_KNOWN: &[(&str, &[&str])] = &[
        ("Option", &["core", "option", "Option"]),
        ("Iterator", &["core", "iterator", "Iterator"]),
        ("ToIterator", &["core", "iterator", "ToIterator"]),
    ];
    WELL_KNOWN
        .iter()
        .find(|(known, _)| *known == name.as_ref())
        .map(|(_, path)| path.iter().map(|s| Ident(s.to_string())).collect())
}

