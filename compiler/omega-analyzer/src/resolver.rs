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

/// A concrete cross-module lookup result -- either a type (a struct, found
/// via a qualified type reference) or a value (a function/extern/global,
/// found via a qualified place). This is deliberately the same shape
/// `VarBinding`/`find_defined_type` already split into locally; a foreign
/// lookup just needs both possibilities in one enum instead of two separate
/// local tables, since the caller doesn't yet know which kind it is.
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
/// its syntax alone (`import a::b::c;` is identical text whether `c` is a
/// submodule of `a::b` or an item inside it), so this is the answer a
/// `ModuleResolver` gives back after actually checking the module tree.
#[derive(Debug, Clone)]
pub enum ImportTarget {
    /// `path` names a real module -- the imported name binds to that whole
    /// namespace (`import mymodule;` then `mymodule::thing::foo()`).
    Module(Vec<Ident>),
    /// `path`'s last segment names an item inside the module formed by the
    /// rest of the path -- the imported name binds directly to that item
    /// (`import mymodule::foo;` then bare `foo()`). Carries its own absolute
    /// path alongside the eagerly-resolved `ResolvedItem` snapshot: that
    /// snapshot was always produced with `indirect = true` (see
    /// `omega_driver`'s import resolution -- classifying "what does
    /// this alias mean" never itself embeds anything inline), which is fine
    /// for every value-position consumer (a call, a literal construction --
    /// never inline-embedded either way) but wrong to trust as-is for a
    /// *type-annotation* position, where the real `indirect` varies by
    /// where the annotation sits (a struct field vs. a pointer's pointee).
    /// The absolute path lets that one consumer (`Context::resolve_type`'s
    /// `Type::Named` unqualified-alias branch) re-resolve through
    /// `ModuleResolver::resolve_item` with its own real `indirect` instead,
    /// so a mutual by-value struct cycle reached through a bare import
    /// alias still gets caught exactly like one reached through a qualified
    /// path already was.
    Item(Vec<Ident>, ResolvedItem),
    /// `path`'s last segment names a *generic* item (struct or function) --
    /// unlike `Item`, this is never eagerly resolved to a `ResolvedItem`:
    /// importing supplies no type arguments (those only ever appear at a use
    /// site, e.g. `List<u32>` or `sum_generic(1, 2)`), so there is nothing
    /// concrete to build yet. Just the absolute path, to be substituted in
    /// for the alias wherever it's later referenced with concrete arguments
    /// (see `Context::generic_aliases`).
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
    /// signature (e.g. two structs in different modules referencing each
    /// other by value) -- `path` is the cycle, in the order it was
    /// discovered, ending back where it started.
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
    /// through one or more other structs -- possibly in other modules --
    /// entirely by value, with no pointer anywhere along the cycle. Such a
    /// type has no finite size (the same shape Rust rejects as E0072); this
    /// is the one global, item-granular query
    /// (`omega_driver::Driver::ensure_item`) replaces the old module-
    /// granularity `Cycle` above for -- see its doc comment for why a
    /// *pointer* reference to something still being resolved is never an
    /// error, only a direct, by-value one.
    RecursiveTypeWithoutIndirection {
        module: Vec<Ident>,
        item: Ident,
    },
    /// `item` (in `module`) failed its own signature/body analysis -- the
    /// real diagnostics were already recorded against that module elsewhere
    /// (see `omega_driver`'s per-module diagnostic sink); this is just a
    /// lightweight marker so a *reference* to the failed item can itself
    /// fail cleanly, without duplicating or re-deriving the underlying
    /// error here.
    ItemFailed {
        module: Vec<Ident>,
        item: Ident,
    },
    /// `item` (in `module`) declares `expected` generic parameters, but was
    /// referenced with `found` type arguments -- covers both a generic item
    /// referenced with no arguments at all (a bare `Type::Named`, `found:
    /// 0`) and a `Type::Generic`/instantiation with the wrong count.
    GenericArgCountMismatch {
        module: Vec<Ident>,
        item: Ident,
        expected: usize,
        found: usize,
    },
    /// A bound generic (`T: Animal`) was instantiated with a concrete type
    /// that doesn't nominally implement `spec` -- `missing` names every
    /// spec function the type doesn't provide (own or default). Also used
    /// for a `spec *Animal` coercion from a concrete pointer whose pointee
    /// doesn't implement the spec.
    SpecNotImplemented {
        type_name: String,
        spec: Ident,
        missing: Vec<Ident>,
    },
    /// `spec` (in `module`) transitively depends on itself through one or
    /// more other specs' own dependency lists (`spec A : B; spec B : A;`)
    /// -- the spec-declaration analog of `RecursiveTypeWithoutIndirection`
    /// above, needed because `ModuleResolver::spec_declaration` bypasses
    /// `ensure_item` entirely (see its own doc comment) and so has no
    /// module-level `Cycle` guard to fall back on; it keeps its own,
    /// narrower cycle guard instead.
    SpecDependencyCycle {
        module: Vec<Ident>,
        spec: Ident,
    },
    /// `item` (in `module`) is a `spec T`-returning function whose own
    /// return-type inference (see `omega_driver::Driver::
    /// resolve_spec_return_function`) transitively calls back into itself
    /// before its own signature is done -- the mutual-recursion analog of
    /// `SpecDependencyCycle` above, needed for the identical reason: this
    /// inference bypasses the ordinary phase-1-before-phase-2 barrier that
    /// makes an *ordinary* function's own self-recursion safe (its
    /// signature is always `Done` before any body, including its own, is
    /// ever checked -- not true here, since discovering *this* signature
    /// requires checking this body first).
    SpecReturnTypeRecursion {
        module: Vec<Ident>,
        item: Ident,
    },
    /// A bare, unqualified name matched more than one `core` submodule's
    /// own exposed top-level item, while resolving core's ambient-prelude
    /// fallback (`ModuleResolver::ambient_core_candidates`) -- unlike every
    /// other `ResolveError`, this can only ever be produced by that one
    /// query. `candidates` is every module that exposes `name`, in
    /// discovery order. Always recoverable: the fully-qualified path
    /// (`candidates[i]::name`) is unaffected and still resolves.
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
            Self::SpecReturnTypeRecursion { module, item } => write!(
                f,
                "cannot infer '{}::{}'s 'spec T' return type: it recursively depends on itself",
                join(module),
                item.as_ref()
            ),
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
/// module-qualified. Everything module-tree-shaped -- submodule-vs-item
/// disambiguation at each `::`, filesystem lookups, cross-module caching,
/// cycle detection -- lives entirely in the implementation (`omega-driver`);
/// this crate never sees a filesystem or a cache, only ever asks these two
/// questions.
pub trait ModuleResolver {
    /// The module that authored tokens emitted by this macro invocation.
    /// `None` means the path was written directly in the module being
    /// analyzed. Keeping this query on the resolver lets parser provenance
    /// stay an opaque id rather than coupling the parser to driver modules.
    fn macro_origin_module(&self, origin: Origin) -> Option<Vec<Ident>>;

    /// The visibility declared on the macro that emitted `origin`, when this
    /// is a macro-authored token.
    fn macro_origin_visibility(&self, origin: Origin) -> Option<Visibility>;

    /// An item's declared visibility, without applying an accessor check.
    fn declared_item_visibility(&mut self, absolute_path: &[Ident]) -> Option<Visibility>;

    /// What `alias` means as an import in `module_path`, resolved lazily and
    /// memoized per `(module_path, alias)` pair (not per whole module) --
    /// the fix for a real false-cycle bug a whole-module-granular version of
    /// this used to have: two modules whose *unrelated* items happened to
    /// cross-import each other's module would deadlock resolving each
    /// other's *entire* import list, even though the specific items in
    /// question never referenced each other. `Ok(None)` means `module_path`
    /// has no `import` statement binding `alias` at all -- the caller's own
    /// "assume this name is my own module's item" fallback applies, exactly
    /// as if this had never been called. Called on demand, the first time a
    /// name lookup that isn't satisfied locally actually needs to know
    /// whether it's an import alias -- never eagerly for a module's whole
    /// import list up front (see `Analyzer::new`, which no longer takes a
    /// pre-resolved import list at all).
    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError>;

    /// `core`'s ambient-prelude fallback for a bare name (see
    /// `docs/10-modules-and-linkage.md`'s "core is a prelude" section) --
    /// consulted only after ordinary local/import resolution of `name`
    /// already failed, exactly like the narrower, now-superseded
    /// `context::ambient_core_path` this replaces. `Ok(None)`: no `core`
    /// submodule exposes `name` at all (or `accessor` is itself inside
    /// `core`, which never gets this fallback -- its own submodules still
    /// need real imports among themselves). `Ok(Some(path))`: exactly one
    /// does. `Err(AmbiguousAmbientName)`: two or more do -- always
    /// recoverable by writing the fully-qualified path instead.
    fn ambient_core_candidates(
        &mut self,
        accessor: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<Ident>>, ResolveError>;

    /// Every alias a module's own `import` statements bind, purely for "did
    /// you mean" typo suggestions (`Context::similar_module_alias`) -- cheap
    /// and resolution-free (the raw alias *names* are known the moment a
    /// module is indexed, long before any of them are actually resolved).
    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident>;

    /// `alias`'s own already-computed absolute target path in `module_path`
    /// (`import lib::pick;` -> `["lib", "pick"]`), plus whether that import
    /// was written `reveal` -- purely structural and resolution-free, like
    /// `import_alias_names`, deliberately **not** going through
    /// `resolve_import_alias`/`ensure_item` at all. `resolve_import_alias`
    /// eagerly resolves to *one* concrete item, which is exactly wrong for
    /// an alias to an *overloaded* name (`ModuleResolver::
    /// function_overload_signatures`'s whole reason to exist): picking a
    /// single winner before the call's own argument types are known would
    /// silently commit to whichever overload happened to be indexed first,
    /// regardless of arity/types -- and regardless of `reveal`, since a
    /// `reveal`-only-visible overload could never even be considered.
    /// `Analyzer::resolve_overloaded_call`'s unqualified-alias case uses
    /// this instead, mirroring how its own unqualified-*own-module* case
    /// already builds an absolute path directly rather than resolving
    /// through an item query first. `Ok(None)` means "not an alias at all"
    /// (same convention as `resolve_import_alias`).
    fn raw_import_absolute_path(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<(Vec<Ident>, bool)>, ResolveError>;

    /// Called for *any* named-type or place reference that isn't satisfied
    /// by a local (function-body-level) scope -- including a same-module
    /// top-level reference, with `absolute_path`'s module prefix supplied
    /// implicitly by the caller. There is no longer an architectural
    /// difference between "same-module" and "cross-module" here; both are
    /// this one query, item-granular and memoized
    /// (`omega_driver::Driver::ensure_item`).
    ///
    /// `indirect` is true whenever the reference sits somewhere that never
    /// embeds its referent inline into another type's layout -- behind a
    /// pointer, or a function's own param/return types -- as opposed to a
    /// struct field or `SizedArray` element, which do. This is what lets a
    /// self/mutually-referencing *pointer* field (anywhere, even across
    /// modules) resolve while it's still mid-collection, while a direct,
    /// by-value reference to something still mid-collection is rejected as
    /// `ResolveError::RecursiveTypeWithoutIndirection` (a genuine
    /// infinite-size type) instead of silently built.
    ///
    /// `type_args` is the concrete substitution for a generic item's own
    /// declared type parameters -- empty for an ordinary, non-generic item
    /// (the overwhelmingly common case; every non-generic call site passes
    /// `&[]`), or the arguments a generic reference was instantiated with
    /// (`List<u32>`'s `[u32]`, or a generic function call's argument-deduced
    /// substitution -- see `Analyzer::resolve_generic_call`). A count
    /// mismatch against the item's own declared generic parameter list
    /// (including a non-empty declared list against an empty `type_args`,
    /// i.e. a bare reference to a generic item with no arguments at all) is
    /// `ResolveError::GenericArgCountMismatch`.
    ///
    /// `accessor_module_path` is the *querying* module -- the one piece of
    /// context this query didn't used to need, back when every item was
    /// implicitly public; now the target's own declared `exposed`/
    /// `internal`/(default hidden) visibility is checked against it on
    /// every call (see `omega_driver::Driver::ensure_item`), returning
    /// `ResolveError::NotVisible` on denial -- unless `bypass` is set
    /// (`reveal`, see `omega_analyzer::analysis::Analyzer::reveal_stack`),
    /// which allows the access through regardless. `bypass` never affects
    /// what's cached; it only ever suppresses this one call's own
    /// `NotVisible` result.
    fn resolve_item(
        &mut self,
        accessor_module_path: &[Ident],
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
        bypass: bool,
    ) -> Result<ResolvedItem, ResolveError>;

    /// Whether `absolute_path` is visible from `accessor_module_path`
    /// *ignoring* any `reveal` bypass -- the one query `Analyzer` uses, after
    /// a bypassed `resolve_item` call succeeds, to decide whether that bypass
    /// actually mattered (see `AnalysisWarningKind::UnnecessaryReveal`).
    /// Answered from the item's own *declaration*, so it needs no prior
    /// resolution and is identical for every instantiation of a generic
    /// template. `false` for a name that doesn't resolve at all.
    fn is_item_visible(&mut self, accessor_module_path: &[Ident], absolute_path: &[Ident]) -> bool;

    /// A *raw*, unresolved view of a generic function's own declared
    /// signature -- just enough for duck-typed argument-driven type
    /// inference at a call site (see `Analyzer::resolve_generic_call`), with
    /// no analysis triggered and no instantiation attempted. `Ok(None)` for
    /// anything that isn't a generic function -- including a non-generic
    /// item, a generic *struct*, or a name that doesn't resolve at all --
    /// deferring all of those diagnoses to the ordinary (non-generic) call
    /// path, which re-derives them identically.
    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError>;

    /// A *raw*, unresolved view of a generic struct/union/enum-variant's own
    /// declared field shape -- just enough for duck-typed, field-driven type
    /// inference at a literal construction site (`Name { field = value; }`,
    /// see `Analyzer::resolve_literal_item`), with no analysis triggered and
    /// no instantiation attempted. Exactly `generic_function_signature`'s own
    /// contract, one level down: `Ok(None)` for anything that isn't a
    /// generic struct/union/enum -- including a non-generic item, a name
    /// that doesn't resolve at all, or (when `variant` is `Some`) an enum
    /// whose variant name doesn't exist -- deferring all of those diagnoses
    /// to the ordinary literal-resolution path, which re-derives them
    /// identically. `variant` is `None` for a struct/union target, `Some`
    /// (always, and always validated) for an enum variant target.
    fn generic_literal_signature(
        &mut self,
        absolute_path: &[Ident],
        variant: Option<&Ident>,
    ) -> Result<Option<GenericLiteralSignature>, ResolveError>;

    /// A *raw*, unresolved view of a generic struct/union/enum's own
    /// declared `self`-less (static) function named `function_name` --
    /// just enough for duck-typed, argument-driven type inference at a
    /// call site (`Owner::function(args)`, no explicit `<...>`, see
    /// `Analyzer::resolve_generic_static_call`), with no analysis
    /// triggered and no instantiation attempted. `Ok(None)` for anything
    /// that isn't a generic struct/union/enum, or that has no matching
    /// static function under that name -- deferring both diagnoses to the
    /// ordinary call path, which re-derives them identically.
    fn generic_static_function_signature(
        &mut self,
        owner_absolute: &[Ident],
        function_name: &Ident,
    ) -> Result<Option<GenericStaticFunctionSignature>, ResolveError>;

    /// `name`'s every overload candidate in `module_path`, each already
    /// paired with the `HirId` a callee place root needs -- an escape hatch
    /// alongside `resolve_item` exactly like `generic_function_signature`
    /// is, and for the identical reason: an overloaded name can't be
    /// addressed by `resolve_item`'s single-result `(absolute_path,
    /// type_args)` key at all (nothing about the *name* alone picks one
    /// candidate; only the call's own argument types do, at the call
    /// site -- see `Analyzer::resolve_overloaded_call`). `Ok(None)` means
    /// "not an overloaded name" (zero or exactly one candidate) -- callers
    /// fall through to the ordinary `resolve_item` path unchanged in that
    /// case, so this never affects behavior for the overwhelmingly common
    /// non-overloaded name. Each candidate's own declared `Visibility` rides
    /// alongside its signature -- `resolve_overload` itself picks a winner
    /// purely by argument-type fit, with no notion of visibility at all;
    /// `resolve_overloaded_call` checks the *winner's* own `Visibility`
    /// after the fact, exactly like the single-candidate path
    /// (`resolve_type_member`) already does.
    fn function_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<OverloadCandidates>, ResolveError>;

    /// Mints a fresh `HirId` with no corresponding HIR node of its own --
    /// used for a spec-default method instantiated for a concrete
    /// implementor that didn't override it (see
    /// aggregate signature resolution),
    /// exactly the same minting `omega_driver::Driver::compute_item`
    /// already does internally for a generic instantiation's own identity,
    /// surfaced here so `Analyzer` (which has no minting of its own) can
    /// request one too.
    fn fresh_synthetic_id(&mut self) -> HirId;

    /// The name of a top-level item in `module_path` most similar to
    /// `target` (see `crate::similarity::best_match`), drawn only from
    /// `namespace` -- the "did you mean" candidate for a reference that
    /// resolved to nothing. Only the resolver can answer this: the analyzer
    /// never holds a module-wide name list (items are resolved one at a
    /// time, on demand), so scope-level searches alone would miss every
    /// top-level item. Purely advisory (error path only): `None` when
    /// nothing is close enough, or when the module can't even be indexed.
    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident>;

    /// A spec's own canonical, args-independent declaration -- its
    /// `dependencies`/`functions` list, structurally, with no concrete
    /// `Self` or generic-argument substitution attempted (both stay raw,
    /// exactly like `generic_function_signature`'s own contract). An escape
    /// hatch alongside `resolve_item` for the identical reason that method
    /// exists: a *generic* spec's own dependency list (`spec Foo<T> : Bar
    /// <T>`) needs to know *which* spec `Bar` is before `T` is bound to
    /// anything concrete, and a spec's cell content never actually varies
    /// by type arguments in the first place -- `flatten_spec` always
    /// receives its concrete args explicitly from whichever call site is
    /// doing the flattening (a bound's own `<i32>`, a conform declaration's
    /// own args), never derived from the cell's own stored state. `Ok(None)`
    /// for anything that isn't a spec -- including a name that doesn't
    /// resolve at all -- deferring that diagnosis to the ordinary
    /// `resolve_item` path, which re-derives it identically for a caller's
    /// own concrete reference. **Deliberately visibility-blind** (like
    /// `resolve_item`'s own cache, per its doc comment) -- the one canonical
    /// cell is shared by every caller regardless of who's asking, so an
    /// accessor-aware visibility check must be re-run by the caller on every
    /// use (see `Analyzer::resolve_spec_dependencies`), never baked in here.
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

    /// A `comp` evaluation's one hook into the driver (see
    /// `crate::comp_eval::CompFunctionResolver`, which this trait re-exposes
    /// through `omega_driver::Driver`'s own `ModuleResolver` impl, matching
    /// every other cross-item query here): the checked body behind
    /// `decl_id`, found by identity alone -- a generic instantiation mints
    /// its own fresh synthetic id at resolution time (`identity_for`), so
    /// `decl_id` alone is already exact identity for one specific
    /// instantiation, with no separate module path or type-args needed.
    ///
    /// Unlike every other query above, this can be asked for an item whose
    /// *body* has never been checked yet, regardless of where whole-program
    /// compilation's own two-phase sweep currently stands -- the
    /// implementation is responsible for checking (and memoizing) it on
    /// demand, exactly like a generic instantiation's body already is.
    /// `Ok(None)` means `decl_id` doesn't name an ordinary checked function
    /// at all (most likely an `extern` declaration) -- distinguished from
    /// `Err` (a genuine resolution failure) so a `comp` evaluation can
    /// report the precise "calling an extern" reason instead of a generic
    /// failure.
    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<crate::checked::CheckedFunctionDef>, ResolveError>;

    /// A top-level `comp` binding's already-evaluated value, found by its
    /// own `decl_id` -- the cross-item counterpart of `Context::
    /// comp_value`, which only ever holds *local* bindings. `Analyzer::
    /// analyze_place_read` falls back to this the moment a local lookup
    /// misses, so a reference to a `comp` global (declared by some other
    /// item, resolved the ordinary cross-item way -- see `ModuleResolver::
    /// resolve_item`) substitutes exactly like a local one does. `None`
    /// for any `decl_id` that isn't a `comp` global's own -- including
    /// every local `comp` binding, which `Context::comp_value` alone
    /// already answers, so this is never even asked about those.
    fn resolve_comp_value(&mut self, decl_id: HirId) -> Option<ConstValue>;
}

/// Which namespace a "did you mean" suggestion should draw from -- a type
/// position must never suggest a function, and vice versa (a wrong hint is
/// worse than none). See `ModuleResolver::similar_item_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemNamespace {
    /// Functions, globals, externs.
    Value,
    /// Structs (generic templates included).
    Type,
}

/// One overloaded name's candidates: each declaration's own id, resolved
/// signature, and declared visibility. Nothing about the *name* picks a
/// winner -- only a call's own argument types can, at the call site.
pub type OverloadCandidates = Vec<(HirId, ResolvedFunctionType, Visibility)>;

/// See `ModuleResolver::generic_function_signature`.
#[derive(Debug, Clone)]
pub struct GenericSignature {
    pub generics: Vec<Ident>,
    /// Each entry in `generics`' own declared default, parallel by index
    /// (`None` for a generic with no default) -- feeds
    /// `Analyzer::infer_generic_args`'s eager, per-argument precedence
    /// resolution (explicit > default > inference).
    pub defaults: Vec<Option<Type>>,
    pub params: Vec<Type>,
}

/// See `ModuleResolver::generic_literal_signature`.
#[derive(Debug, Clone)]
pub struct GenericLiteralSignature {
    pub generics: Vec<Ident>,
    /// Parallel to `generics`, see `GenericSignature::defaults`.
    pub defaults: Vec<Option<Type>>,
    /// Raw, unresolved declared field types, in the same order
    /// `Analyzer::analyze_struct_literal`'s own `declared` already uses: a
    /// struct's/union's own `fields`, or an enum variant's `dynamic_fields`
    /// chained with the variant's own `fields`.
    pub fields: Vec<(Ident, Type)>,
}

/// See `ModuleResolver::generic_static_function_signature`.
#[derive(Debug, Clone)]
pub struct GenericStaticFunctionSignature {
    pub owner_generics: Vec<Ident>,
    /// Parallel to `owner_generics`, see `GenericSignature::defaults`.
    /// `function_generics` gets no equivalent -- it's never resolved at all
    /// today (see its own doc comment just below), so a default on one
    /// would have nothing to plug into.
    pub owner_defaults: Vec<Option<Type>>,
    /// The function's own declared generics, if any -- almost always
    /// empty. Kept (not silently dropped) so `resolve_generic_static_call`
    /// can make an explicit, honest decision about them (decline, today)
    /// instead of quietly assuming none exist.
    pub function_generics: Vec<Ident>,
    pub params: Vec<Type>,
}
