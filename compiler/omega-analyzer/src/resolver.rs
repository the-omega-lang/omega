use crate::checked::Storage;
use crate::resolved_type::{
    ConstValue, ResolvedConformance, ResolvedFunctionType, ResolvedGap, ResolvedMethod,
    ResolvedSpecType, ResolvedType,
};
use omega_hir::HirId;
use omega_parser::prelude::{Ident, Origin, Type, Visibility};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// A concrete cross-module lookup result -- a type, a value (function/
/// extern/global), or a gap; the caller doesn't yet know which kind a
/// qualified reference names.
#[derive(Debug, Clone)]
pub enum ResolvedItem {
    Type(ResolvedType),
    Value {
        r#type: ResolvedType,
        storage: Storage,
        decl_id: HirId,
        mutable: bool,
    },
    Gap(Rc<ResolvedGap>),
}

/// What an `import` statement's path actually names -- not decidable from
/// syntax alone (`import a::b::c;` is identical text whether `c` is a
/// submodule of `a::b` or an item inside it), so this is what
/// `ModuleResolver` answers after checking the module tree.
#[derive(Debug, Clone)]
pub enum ImportTarget {
    /// `path` names a real module -- the imported name binds to that whole
    /// namespace (`import mymodule;` then `mymodule::thing::foo()`).
    Module(Vec<Ident>),
    /// `path`'s last segment names an item inside the module formed by the
    /// rest of the path (`import mymodule::foo;` then bare `foo()`).
    /// Carries its own absolute path alongside the eagerly-resolved
    /// `ResolvedItem` snapshot, since that snapshot was always produced
    /// with `indirect = true` and is wrong to trust as-is for a
    /// type-annotation position (see `Context::resolve_type`'s
    /// `Type::Named` unqualified-alias branch, which re-resolves through
    /// `resolve_item` with the real `indirect`).
    Item(Vec<Ident>, ResolvedItem),
    /// `path`'s last segment names a *generic* item -- unlike `Item`,
    /// never eagerly resolved: importing supplies no type arguments (only
    /// a use site does), so there's nothing concrete to build yet. Just
    /// the absolute path, substituted in wherever referenced with concrete
    /// arguments (see `Context::generic_aliases`).
    GenericItem(Vec<Ident>),
}

#[derive(Debug, Clone)]
pub enum ResolveError {
    UnknownModule(Vec<Ident>),
    /// `import extern::name::...;` where `name` wasn't registered via
    /// `--extern=name:path` on the command line.
    UnknownExtern(Ident),
    UnknownItem {
        module: Vec<Ident>,
        item: Ident,
    },
    NotVisible {
        module: Vec<Ident>,
        item: Ident,
    },
    /// A module's signature transitively requires its own, still-in-progress
    /// signature -- `path` is the cycle, ending back where it started.
    Cycle(Vec<Vec<Ident>>),
    /// Two filesystem entries (a file and a directory) claim the same
    /// module name at the same level.
    AmbiguousModule(Vec<Ident>),
    /// `path` resolved to a real file, but reading or parsing it failed --
    /// an I/O error, or a syntax error in the imported file itself.
    LoadFailed {
        path: Vec<Ident>,
        message: String,
    },
    /// `item` (in `module`) is a struct that includes itself, directly or
    /// through other structs, entirely by value with no pointer anywhere
    /// in the cycle -- infinite size (Rust's E0072). Replaces the old
    /// module-granularity `Cycle` for this case; see `resolve_item`'s
    /// `indirect` doc for why a pointer reference to something still
    /// resolving is fine.
    RecursiveTypeWithoutIndirection {
        module: Vec<Ident>,
        item: Ident,
    },
    /// `item` (in `module`) failed its own signature/body analysis -- the
    /// real diagnostic was already recorded elsewhere. Just a marker so a
    /// reference to the failed item fails cleanly too, without
    /// re-deriving the error.
    ItemFailed {
        module: Vec<Ident>,
        item: Ident,
    },
    /// `item` (in `module`) declares `expected` generic parameters, but was
    /// referenced with `found` type arguments -- covers both a bare
    /// reference with none at all (`found: 0`) and an instantiation with
    /// the wrong count.
    GenericArgCountMismatch {
        module: Vec<Ident>,
        item: Ident,
        expected: usize,
        found: usize,
    },
    /// A bound generic (`T: Animal`) was instantiated with a concrete type
    /// that doesn't nominally implement `spec` -- `missing` names every
    /// unmet spec function. Also used for a `spec *Animal` coercion from a
    /// pointer whose pointee doesn't implement the spec.
    SpecNotImplemented {
        type_name: String,
        spec: Ident,
        missing: Vec<Ident>,
    },
    /// `spec` (in `module`) transitively depends on itself (`spec A : B;
    /// spec B : A;`) -- the spec-declaration analog of
    /// `RecursiveTypeWithoutIndirection`, needed because
    /// `spec_declaration` bypasses `ensure_item` and has no module-level
    /// `Cycle` guard to fall back on.
    SpecDependencyCycle {
        module: Vec<Ident>,
        spec: Ident,
    },
    /// A bare, unqualified name matched more than one `core` submodule's
    /// own exposed item while resolving core's ambient-prelude fallback --
    /// `candidates` is every module exposing `name`. Always recoverable
    /// via the fully-qualified path (`candidates[i]::name`).
    AmbiguousAmbientName {
        name: Ident,
        candidates: Vec<Vec<Ident>>,
    },
}

fn join(path: &[Ident]) -> String {
    path.iter()
        .map(|i| i.as_ref())
        .collect::<Vec<_>>()
        .join("::")
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModule(path) => write!(f, "cannot find module '{}'", join(path)),
            Self::UnknownExtern(name) => write!(
                f,
                "no extern dependency named '{}' (missing --extern={}:<path>?)",
                name.as_ref(),
                name.as_ref()
            ),
            Self::UnknownItem { module, item } => {
                write!(
                    f,
                    "cannot find '{}' in module '{}'",
                    item.as_ref(),
                    join(module)
                )
            }
            Self::NotVisible { module, item } => {
                write!(
                    f,
                    "'{}::{}' is not visible here",
                    join(module),
                    item.as_ref()
                )
            }
            Self::Cycle(path) => write!(
                f,
                "cyclic module dependency: {}",
                path.iter()
                    .map(|p| join(p))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::AmbiguousModule(path) => write!(
                f,
                "module '{}' is ambiguous (both a file and a directory claim this name)",
                join(path)
            ),
            Self::LoadFailed { path, message } => {
                write!(f, "failed to load module '{}': {message}", join(path))
            }
            Self::RecursiveTypeWithoutIndirection { module, item } => write!(
                f,
                "recursive type '{}::{}' has infinite size",
                join(module),
                item.as_ref()
            ),
            Self::ItemFailed { module, item } => {
                write!(
                    f,
                    "cannot use '{}::{}' because of its own error",
                    join(module),
                    item.as_ref()
                )
            }
            Self::GenericArgCountMismatch {
                module,
                item,
                expected,
                found,
            } => write!(
                f,
                "'{}::{}' expects {expected} type argument(s), found {found}",
                join(module),
                item.as_ref()
            ),
            Self::SpecNotImplemented {
                type_name,
                spec,
                missing,
            } => write!(
                f,
                "'{type_name}' does not implement spec '{}' (missing: {})",
                spec.as_ref(),
                missing
                    .iter()
                    .map(Ident::as_ref)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::SpecDependencyCycle { module, spec } => {
                write!(
                    f,
                    "spec '{}::{}' depends on itself",
                    join(module),
                    spec.as_ref()
                )
            }
            Self::AmbiguousAmbientName { name, candidates } => write!(
                f,
                "'{}' is ambiguous: it's exposed by more than one core module ({})",
                name.as_ref(),
                candidates
                    .iter()
                    .map(|c| join(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// What `omega-analyzer` needs from the outside world to resolve anything
/// module-qualified. Module-tree mechanics (disambiguation, filesystem,
/// caching, cycle detection) live in `omega-driver`; this crate only asks
/// these queries.
pub trait ModuleResolver {
    /// The module that authored tokens emitted by this macro invocation.
    /// `None` means the path was written directly in the module being
    /// analyzed.
    fn macro_origin_module(&self, origin: Origin) -> Option<Vec<Ident>>;

    /// The visibility declared on the macro that emitted `origin`, when this
    /// is a macro-authored token.
    fn macro_origin_visibility(&self, origin: Origin) -> Option<Visibility>;

    /// An item's declared visibility, without applying an accessor check.
    fn declared_item_visibility(&mut self, absolute_path: &[Ident]) -> Option<Visibility>;

    /// What `alias` means as an import in `module_path`, resolved lazily
    /// and memoized per `(module_path, alias)` pair, not per whole module.
    /// `Ok(None)` means no `import` binds `alias` at all -- the caller's
    /// own "assume this is my own module's item" fallback applies. Called
    /// on demand, the first time a name lookup needs to know whether it's
    /// an import alias -- never eagerly for a module's whole import list.
    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError>;

    /// `core`'s ambient-prelude fallback for a bare name (see
    /// `docs/10-modules-and-linkage.md`'s "core is a prelude" section),
    /// consulted only after ordinary local/import resolution fails.
    /// `Ok(None)`: no `core` submodule exposes `name` (or `accessor` is
    /// itself inside `core`, which never gets this fallback). `Ok(Some
    /// (path))`: exactly one does. `Err(AmbiguousAmbientName)`: more than
    /// one does.
    fn ambient_core_candidates(
        &mut self,
        accessor: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<Ident>>, ResolveError>;

    /// Every alias a module's own `import` statements bind, purely for
    /// "did you mean" typo suggestions (`Context::similar_module_alias`) --
    /// cheap and resolution-free.
    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident>;

    /// `alias`'s own already-computed absolute target path in
    /// `module_path` (`import lib::pick;` -> `["lib", "pick"]`), plus
    /// whether that import was written `reveal` -- structural and
    /// resolution-free, deliberately not going through
    /// `resolve_import_alias` (which eagerly resolves to *one* concrete
    /// item, wrong for an alias to an *overloaded* name where only the
    /// call's own argument types can pick a winner).
    /// `Analyzer::resolve_overloaded_call`'s unqualified-alias case uses
    /// this instead. `Ok(None)` means "not an alias at all" (same
    /// convention as `resolve_import_alias`).
    fn raw_import_absolute_path(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(Vec<Ident>, bool)>, ResolveError>;

    /// Called for *any* named-type or place reference not satisfied by a
    /// local scope, including a same-module top-level reference.
    /// Item-granular and memoized (`omega_driver::Driver::ensure_item`).
    ///
    /// `indirect` is true when the reference never embeds its referent
    /// inline into another type's layout (behind a pointer, or a
    /// function's param/return types), unlike a struct field or
    /// `SizedArray` element, which do. This lets a self/mutually-
    /// referencing pointer field resolve while still mid-collection, while
    /// a direct by-value reference to something still mid-collection is
    /// rejected as `RecursiveTypeWithoutIndirection`.
    ///
    /// `type_args` is the concrete substitution for a generic item's
    /// declared type parameters -- empty for a non-generic item. A count
    /// mismatch is `GenericArgCountMismatch`.
    ///
    /// `accessor_module_path` is the querying module, checked against the
    /// target's declared visibility, returning `NotVisible` on denial
    /// unless `bypass` is set (`reveal`). `bypass` never affects what's
    /// cached, only this call's own `NotVisible` result.
    fn resolve_item(
        &mut self,
        accessor_module_path: &[Ident],
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedItem, ResolveError>;

    /// Whether `absolute_path` is visible from `accessor_module_path`,
    /// ignoring any `reveal` bypass -- used after a bypassed `resolve_item`
    /// succeeds to decide whether the bypass actually mattered (see
    /// `AnalysisWarningKind::UnnecessaryReveal`). `false` if the name
    /// doesn't resolve at all.
    fn is_item_visible(&mut self, accessor_module_path: &[Ident], absolute_path: &[Ident]) -> bool;

    /// A raw, unresolved view of a generic function's declared signature --
    /// enough for duck-typed argument-driven inference at a call site, with
    /// no analysis or instantiation triggered. `Ok(None)` for anything that
    /// isn't a generic function, deferring diagnosis to the ordinary call
    /// path.
    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError>;

    /// A raw, unresolved view of a generic struct/union/enum-variant's own
    /// declared field shape -- enough for duck-typed, field-driven
    /// inference at a literal construction site (`Name { field = value; }`),
    /// with no analysis or instantiation attempted. `generic_function_
    /// signature`'s own contract, one level down. `variant` is `None` for a
    /// struct/union target, `Some` for an enum variant target.
    fn generic_literal_signature(
        &mut self,
        absolute_path: &[Ident],
        variant: Option<&Ident>,
    ) -> Result<Option<GenericLiteralSignature>, ResolveError>;

    /// A raw, unresolved view of a generic struct/union/enum's own declared
    /// `self`-less (static) function named `function_name` -- enough for
    /// duck-typed, argument-driven inference at a call site
    /// (`Owner::function(args)`, no explicit `<...>`), with no analysis or
    /// instantiation attempted. `Ok(None)` for anything that isn't a
    /// generic type, or has no matching static function.
    fn generic_static_function_signature(
        &mut self,
        owner_absolute: &[Ident],
        function_name: &Ident,
    ) -> Result<Option<GenericStaticFunctionSignature>, ResolveError>;

    /// `name`'s every overload candidate in `module_path`, each already
    /// paired with the `HirId` a callee place root needs -- an escape
    /// hatch alongside `resolve_item`, since an overloaded name can't be
    /// addressed by its single-result `(absolute_path, type_args)` key
    /// (only the call's own argument types pick a candidate). `Ok(None)`
    /// means "not an overloaded name" (zero or one candidate) -- callers
    /// fall through to the ordinary `resolve_item` path unchanged. Each
    /// candidate carries its own declared `Visibility`: `resolve_overload`
    /// picks a winner purely by argument-type fit, and
    /// `resolve_overloaded_call` checks the winner's `Visibility` after
    /// the fact.
    fn function_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<OverloadCandidates>, ResolveError>;

    /// Mints a fresh `HirId` with no corresponding HIR node -- used for a
    /// spec-default method instantiated for an implementor that didn't
    /// override it, mirroring the minting `Driver::compute_item` already
    /// does for a generic instantiation's identity.
    fn fresh_synthetic_id(&mut self) -> HirId;

    /// The name of a top-level item in `module_path` most similar to
    /// `target` (see `crate::similarity::best_match`), drawn only from
    /// `namespace` -- the "did you mean" candidate for a reference that
    /// resolved to nothing. Only the resolver can answer this: the
    /// analyzer never holds a module-wide name list. `None` when nothing
    /// is close enough, or the module can't be indexed.
    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident>;

    /// A spec's own canonical, args-independent declaration -- its
    /// `dependencies`/`functions` list, structurally, with no concrete
    /// `Self` or generic-argument substitution attempted. An escape hatch
    /// alongside `resolve_item`: a *generic* spec's own dependency list
    /// (`spec Foo<T> : Bar<T>`) needs to know *which* spec `Bar` is before
    /// `T` is bound to anything concrete. `Ok(None)` for anything that
    /// isn't a spec. **Deliberately visibility-blind** -- the one
    /// canonical cell is shared by every caller regardless of who's
    /// asking, so an accessor-aware visibility check must be re-run by the
    /// caller on every use (see `Analyzer::resolve_spec_dependencies`).
    fn spec_declaration(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<Rc<RefCell<ResolvedSpecType>>>, ResolveError>;

    /// `receiver`'s inherent primitive methods, if any. Only `core` can
    /// declare these, via `primitive Target { ... }`; generic slice blocks
    /// are instantiated lazily for concrete element types.
    fn primitive_methods(
        &mut self,
        receiver: &ResolvedType,
    ) -> Result<Vec<(Ident, ResolvedMethod)>, ResolveError>;

    fn conformance_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Result<Option<ResolvedConformance>, ResolveError>;

    fn conformances_for_type(
        &mut self,
        target: &ResolvedType,
    ) -> Result<Vec<ResolvedConformance>, ResolveError>;

    /// The conformances of `target` whose spec is one of `spec_ids` --
    /// goal-directed: only templates that can produce one of these specs
    /// are instantiated. Used by `type_implements_spec`'s alias fallback
    /// so satisfying a spec alias mid-proof can't trigger a full sweep.
    fn conformances_for_specs(
        &mut self,
        target: &ResolvedType,
        spec_ids: &[HirId],
    ) -> Result<Vec<ResolvedConformance>, ResolveError>;

    /// A `comp` evaluation's hook into the driver: the checked body
    /// behind `decl_id`, found by identity alone -- a generic
    /// instantiation mints its own fresh synthetic id at resolution time,
    /// so `decl_id` alone is exact identity for one instantiation.
    ///
    /// Unlike every other query above, this can be asked for an item whose
    /// body hasn't been checked yet -- the implementation checks and
    /// memoizes it on demand. `Ok(None)` means `decl_id` doesn't name an
    /// ordinary checked function (most likely an `extern`), distinguished
    /// from `Err` so a `comp` evaluation can report the precise reason.
    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<crate::checked::CheckedFunctionDef>, ResolveError>;

    /// A top-level `comp` binding's already-evaluated value, found by its
    /// own `decl_id` -- the cross-item counterpart of
    /// `Context::comp_value`, which only holds *local* bindings.
    /// `Analyzer::analyze_place_read` falls back to this the moment a
    /// local lookup misses. `None` for any `decl_id` that isn't a `comp`
    /// global's own.
    fn resolve_comp_value(&mut self, decl_id: HirId) -> Option<ConstValue>;
}

/// Which namespace a "did you mean" suggestion should draw from -- a type
/// position must never suggest a function, and vice versa. See
/// `ModuleResolver::similar_item_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemNamespace {
    /// Functions, globals, externs.
    Value,
    /// Structs (generic templates included).
    Type,
}

/// One overloaded name's candidates: each declaration's own id, resolved
/// signature, and declared visibility. Only a call's own argument types
/// pick a winner.
pub type OverloadCandidates = Vec<(HirId, ResolvedFunctionType, Visibility)>;

/// See `ModuleResolver::generic_function_signature`.
#[derive(Debug, Clone)]
pub struct GenericSignature {
    pub generics: Vec<Ident>,
    /// Each entry in `generics`' own declared default, parallel by index
    /// (`None` for no default) -- feeds `Analyzer::infer_generic_args`'s
    /// precedence resolution (explicit > default > inference).
    pub defaults: Vec<Option<Type>>,
    pub params: Vec<Type>,
    /// The declared return type, raw and unresolved -- `finish_generic_call`
    /// unifies it against the call's expected type to seed the
    /// substitution, so a generic named only in its return type
    /// (`lowest<T: Bounded>() => T`) can be called from an expected-type
    /// position (`x : i32 = lowest();`).
    pub return_type: Type,
}

/// See `ModuleResolver::generic_literal_signature`.
#[derive(Debug, Clone)]
pub struct GenericLiteralSignature {
    pub generics: Vec<Ident>,
    /// Parallel to `generics`, see `GenericSignature::defaults`.
    pub defaults: Vec<Option<Type>>,
    /// Raw, unresolved declared field types, in the same order
    /// `Analyzer::analyze_struct_literal`'s own `declared` uses: a
    /// struct's/union's `fields`, or an enum variant's `dynamic_fields`
    /// chained with its own `fields`.
    pub fields: Vec<(Ident, Type)>,
}

/// See `ModuleResolver::generic_static_function_signature`.
#[derive(Debug, Clone)]
pub struct GenericStaticFunctionSignature {
    pub owner_generics: Vec<Ident>,
    /// Parallel to `owner_generics`, see `GenericSignature::defaults`.
    /// `function_generics` gets no equivalent -- it's never resolved
    /// today, so a default would have nothing to plug into.
    pub owner_defaults: Vec<Option<Type>>,
    /// The function's own declared generics, if any -- almost always
    /// empty. Kept so `resolve_generic_static_call` can make an explicit
    /// decision about them (decline, today) instead of assuming none
    /// exist.
    pub function_generics: Vec<Ident>,
    pub params: Vec<Type>,
    /// The declared return type, raw and unresolved, with any `Self` leaf
    /// rewritten to the owner's own generic spelling (`=> Self` becomes
    /// `=> Box<T>`, see `rewrite_self_return` in `omega_driver`).
    /// `finish_generic_static_call` unifies this against the call's
    /// expected type to seed the owner's substitution.
    pub return_type: Type,
}
