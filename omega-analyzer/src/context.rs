use crate::checked::Storage;
use crate::error::TypeResolutionError;
use crate::resolved_type::{ResolvedFunctionType, ResolvedType};
use crate::resolver::{ImportTarget, ItemNamespace, ModuleResolver, ResolveError, ResolvedItem};
use crate::similarity::best_match;
use omega_hir::HirId;
use omega_parser::prelude::*;
use std::collections::HashMap;

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
    pub declared_variables: HashMap<Ident, VarBinding>,
    pub defined_types: HashMap<Ident, ResolvedType>,
}

impl ScopeContext {
    fn new() -> Self {
        Self {
            declared_variables: HashMap::new(),
            defined_types: HashMap::new(),
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
    ) -> Result<ResolvedFunctionType, TypeResolutionError> {
        let params = fntype
            .params
            .into_iter()
            .map(|(ident, typ)| {
                self.resolve_type(typ, resolver, module_path, true)
                    .map(|resolved| (ident, resolved))
            })
            .collect::<Result<Vec<(Ident, ResolvedType)>, TypeResolutionError>>()?;
        Ok(ResolvedFunctionType {
            params,
            return_type: Box::new(self.resolve_type(*fntype.return_type, resolver, module_path, true)?),
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
    fn resolve_absolute_item_path(
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
    pub fn resolve_type(
        &self,
        typ: Type,
        resolver: &mut dyn ModuleResolver,
        module_path: &[Ident],
        indirect: bool,
    ) -> Result<ResolvedType, TypeResolutionError> {
        let resolved = match typ {
            // `Entity::Person` (bare or `mymodule`-qualified) is tried first
            // -- cheap to rule out (`Ok(None)` whenever the prefix doesn't
            // resolve to a plain enum) and must win over the ordinary
            // qualified-path reading below (`Entity` is never itself an
            // imported module alias). See `try_resolve_enum_variant_type`'s
            // own doc comment.
            Type::Named(path) => {
                if let Some(resolved) = self.try_resolve_enum_variant_type(&path, resolver, module_path, indirect)? {
                    resolved
                } else if path.is_unqualified() {
                    if let Some(local) = self.find_defined_type(&path.head) {
                        local.to_owned()
                    } else {
                        // An import alias, lazily resolved -- an ordinary,
                        // non-generic *type* alias resolves outright
                        // (`bind_imported_item` used to pre-materialize this
                        // into `find_defined_type` above; now it's just
                        // resolved on the spot, the first time this miss
                        // actually happens); a *generic* item or *module*
                        // alias falls through to `resolve_item` with no
                        // type arguments, reproducing the exact
                        // `GenericArgCountMismatch`/`NotAType` a bare
                        // reference to either should get; no alias at all
                        // falls through to the implicit own-module
                        // assumption, exactly as before.
                        let alias = resolver
                            .resolve_import_alias(module_path, &path.head)
                            .map_err(TypeResolutionError::ModuleResolution)?;
                        if let Some(ImportTarget::Item(ResolvedItem::Value { .. })) = alias {
                            return Err(TypeResolutionError::NotAType(vec![path.head.clone()]));
                        }
                        if let Some(ImportTarget::Item(ResolvedItem::Type(t))) = alias {
                            t
                        } else {
                            let absolute = match alias {
                                Some(ImportTarget::GenericItem(absolute)) | Some(ImportTarget::Module(absolute)) => {
                                    absolute
                                }
                                _ => module_path.iter().cloned().chain(std::iter::once(path.head.clone())).collect(),
                            };
                            match resolver.resolve_item(&absolute, &[], indirect) {
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
                                        .or_else(|| {
                                            resolver.similar_item_name(module_path, &path.head, ItemNamespace::Type)
                                        });
                                    return Err(TypeResolutionError::UnrecognizedNamedType {
                                        name: path.head.clone(),
                                        similar,
                                    });
                                }
                                Err(e) => return Err(TypeResolutionError::ModuleResolution(e)),
                            }
                        }
                    }
                } else {
                    // A qualified type reference (`mymodule::Foo`) -- `path`'s
                    // head must already be an imported module alias; the rest
                    // is resolved across modules by `resolver`, never locally.
                    let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
                    match resolver
                        .resolve_item(&absolute, &[], indirect)
                        .map_err(TypeResolutionError::ModuleResolution)?
                    {
                        ResolvedItem::Type(t) => t,
                        ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
                    }
                }
            }
            // `Path<Type, ...>` -- a generic item referenced with explicit
            // type arguments (e.g. `List<u32>`). Args are resolved first
            // (always `indirect = true`: naming a type as a generic argument
            // never itself embeds it inline -- the *usage* inside the
            // template body decides that, at instantiation time), then the
            // whole reference is resolved exactly like `Type::Named`'s
            // (minus the local-shadowing check -- a generic item is never a
            // `find_defined_type` entry).
            Type::Generic(path, args) => {
                let resolved_args = args
                    .into_iter()
                    .map(|arg| self.resolve_type(arg, resolver, module_path, true))
                    .collect::<Result<Vec<_>, _>>()?;
                let absolute = self.resolve_absolute_item_path(resolver, &path, module_path)?;
                match resolver
                    .resolve_item(&absolute, &resolved_args, indirect)
                    .map_err(TypeResolutionError::ModuleResolution)?
                {
                    ResolvedItem::Type(t) => t,
                    ResolvedItem::Value { .. } => return Err(TypeResolutionError::NotAType(absolute)),
                }
            }
            // `*[T]` is a slice (a fat pointer), not a thin `Pointer` to an
            // `Array` -- `[T]` alone is unsized, so a pointer to it is
            // necessarily a different, wider representation (data pointer +
            // length), the same reasoning Rust's `&[T]` follows. `*str` is
            // handled *before* recursing into the pointee at all, unlike
            // `*[T]` above: `"str"` is deliberately never registered in
            // `Context::new()`'s `defined_types`, so resolving it as an
            // ordinary pointee would fail with "unrecognized type" before
            // ever reaching a match on the resolved result -- the raw,
            // unresolved AST has to be checked first. Any other pointee
            // resolves via the unchanged logic below (ordinary thin
            // `Pointer`, or `*[T]`'s `Slice` special case).
            Type::Pointer(pointee_type, mutable) => {
                let is_bare_str = matches!(
                    pointee_type.as_ref(),
                    Type::Named(path) if path.is_unqualified() && path.head.as_ref() == "str"
                );
                if is_bare_str {
                    ResolvedType::Str { mutable }
                } else {
                    match self.resolve_type(*pointee_type, resolver, module_path, true)? {
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
            }
            Type::Function(fntyp) => ResolvedType::Function(self.resolve_function_type(fntyp, resolver, module_path)?),
            Type::Array(item_type) => {
                ResolvedType::Array(Box::new(self.resolve_type(*item_type, resolver, module_path, true)?))
            }
            Type::SizedArray(item_type, size) => {
                let size = size
                    .parse::<u32>()
                    .map_err(|_| TypeResolutionError::InvalidArraySize(size.clone()))?;
                ResolvedType::SizedArray(
                    Box::new(self.resolve_type(*item_type, resolver, module_path, indirect)?),
                    size,
                )
            }
            // `spec *Animal`/`spec *mut Animal` -- a dynamic-dispatch
            // trait-object pointer. The pointee is always a spec reference
            // (`Named`/`Generic`), resolved via the ordinary path above
            // (producing `ResolvedType::Spec`); its own generic args (if
            // any, e.g. `Iterator<i32>`) are re-extracted from the raw
            // `Type` here and resolved separately, since `resolve_type`'s
            // own `Generic` arm consumes them internally without handing
            // them back on its `ResolvedType::Spec` result. Never
            // `indirect`-sensitive itself -- a spec object is always a fat
            // pointer, never embedded inline.
            Type::SpecObject(pointee, mutable) => {
                let type_args = match pointee.as_ref() {
                    Type::Generic(_, args) => args.clone(),
                    _ => vec![],
                };
                let resolved_args = type_args
                    .into_iter()
                    .map(|a| self.resolve_type(a, resolver, module_path, true))
                    .collect::<Result<Vec<_>, _>>()?;
                let pointee_name = match pointee.as_ref() {
                    Type::Named(path) | Type::Generic(path, _) => path.head.clone(),
                    _ => Ident("<spec>".to_string()),
                };
                match self.resolve_type(*pointee, resolver, module_path, true)? {
                    ResolvedType::Spec(spec) => {
                        ResolvedType::SpecObject { spec, type_args: resolved_args, mutable }
                    }
                    _ => return Err(TypeResolutionError::NotASpec(pointee_name)),
                }
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
    ) -> Result<Option<ResolvedType>, TypeResolutionError> {
        let Some((variant_name, prefix_tail)) = path.tail.split_last() else { return Ok(None) };
        let prefix = Type::Named(Path { head: path.head.clone(), tail: prefix_tail.to_vec() });
        let Ok(ResolvedType::Enum { cell, variant: None }) = self.resolve_type(prefix, resolver, module_path, indirect) else {
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

