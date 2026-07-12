mod fs_resolve;

use fs_resolve::locate_module;
use omega_analyzer::analysis::{item_id_span, item_name, Analyzer};
use omega_analyzer::checked::{CheckedItem, CheckedModule, ExternFunctionKind, ExternFunctionRef, Storage};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind, AnalysisWarning};
use omega_analyzer::resolved_type::{
    ResolvedEnumType, ResolvedFunctionType, ResolvedStructType, ResolvedType, ResolvedUnionType,
};
use omega_analyzer::resolver::{
    GenericSignature, ImportTarget, ItemNamespace, ModuleResolver, ResolveError, ResolvedItem, Visibility,
};
use omega_analyzer::similarity::best_match;
use omega_diagnostics::{Diagnostic, SourceFile, Span};
use omega_hir::{HirId, HirImport, HirItem, HirModule, ModuleId, SYNTHETIC_MODULE};
use omega_parser::macros::MacroError;
use omega_parser::prelude::{Ident, ImportRoot, ParseError, Path, SourceModule};
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Where to look for a module path on disk, tried in order, first match
/// wins -- deliberately dumb (no per-root package identity/namespacing) so
/// a real package system later just means adding entries and namespacing
/// logic behind this one type, not touching any call site. Exactly one
/// entry today (the entry file's parent directory); see `Driver::new`.
#[derive(Debug, Clone)]
pub struct SearchRoots(pub Vec<PathBuf>);

/// Everything that can go wrong compiling a multi-module program, kept
/// fully structured (never pre-rendered strings) so the CLI can render each
/// finding as an annotated source snippet -- see [`CompileError::module`]/
/// [`CompileError::to_diagnostics`] and `Driver::source_file`.
#[derive(Debug)]
pub enum CompileError {
    /// A module-resolution failure. `importer` is the referencing site (the
    /// importing module and its `import` statement's span) when one is
    /// known -- resolution failures found during reachability discovery
    /// always have one; only a broken *entry* module doesn't.
    Resolve { error: ResolveError, importer: Option<(Vec<Ident>, Span)> },
    /// Syntax errors in one module's own source file.
    Parse { module: Vec<Ident>, errors: Vec<ParseError> },
    /// The module parsed, but macro expansion (run right after parsing,
    /// before HIR lowering) failed.
    MacroExpansion { module: Vec<Ident>, error: MacroError },
    /// Ordinary semantic errors from one module's own signature/body
    /// analysis.
    Analysis { module: Vec<Ident>, errors: Vec<AnalysisError> },
}

impl CompileError {
    /// The module whose source file this error's diagnostics render
    /// against -- `None` only for a resolve error with no known
    /// referencing site.
    pub fn module(&self) -> Option<&[Ident]> {
        match self {
            Self::Resolve { importer, .. } => importer.as_ref().map(|(module, _)| module.as_slice()),
            Self::Parse { module, .. } | Self::MacroExpansion { module, .. } | Self::Analysis { module, .. } => {
                Some(module)
            }
        }
    }

    pub fn to_diagnostics(&self) -> Vec<Diagnostic> {
        match self {
            Self::Resolve { error, importer } => {
                vec![omega_analyzer::error::resolve_error_diagnostic(error, importer.as_ref().map(|&(_, span)| span))]
            }
            Self::Parse { errors, .. } => errors.iter().map(ParseError::to_diagnostic).collect(),
            // A macro error carries no span today (macro expansion runs on
            // spliced token streams, where "one location" is genuinely
            // ambiguous -- definition site vs. invocation site); it renders
            // as a headline-only diagnostic.
            Self::MacroExpansion { error, .. } => vec![Diagnostic::error(error.to_string())],
            Self::Analysis { errors, .. } => errors.iter().map(AnalysisError::to_diagnostic).collect(),
        }
    }
}

/// The result of compiling every module reachable from `entry`: each one's
/// `CheckedModule`, tagged with its absolute module path (codegen needs both
/// for cross-module symbol mangling -- see `omega_codegen::Codegen::generate`),
/// plus every non-fatal finding (see `AnalysisWarning`) across all of them.
pub struct CompiledProgram {
    pub modules: Vec<(Vec<Ident>, CheckedModule)>,
    pub entry: Vec<Ident>,
    /// Each warning tagged with the module it was found in, so the CLI can
    /// render it against the right source file.
    pub warnings: Vec<(Vec<Ident>, AnalysisWarning)>,
    /// Every extern-owned function/method this compilation actually
    /// referenced (see `Driver::collect_extern_functions`) -- `modules`
    /// never contains a body for any of these (an extern module's ordinary
    /// items are scanned, never compiled), so codegen must declare each one
    /// itself, `Linkage::Import`-only, trusting that the *other* `omgc`
    /// invocation compiling that module standalone produces the exact same
    /// mangled symbol (a pure function of module path + name + the item's
    /// own per-module `HirId.local`, deterministic across processes parsing
    /// the same source file) -- never defining a body for it here.
    pub extern_functions: Vec<ExternFunctionRef>,
}

/// One item query's identity: its owning module, its name, and the concrete
/// type arguments it was instantiated with -- empty for an ordinary,
/// non-generic item (the overwhelmingly common case), or a generic item's
/// instantiation-specific substitution (`List<u32>`'s `[u32]`, or a generic
/// function call's argument-deduced one). There is no architectural
/// difference between "an ordinary item" and "a generic instantiation of
/// one" here -- both are this one key shape, just with a different
/// `type_args`; see `Driver::ensure_item`.
type ItemKey = (Vec<Ident>, Ident, Vec<ResolvedType>);

/// An `ItemKey` query's memoized state -- `InProgress` is the white/gray/
/// black cycle guard: an item whose signature collection is already on the
/// call stack is gray, and a second request for it before the first
/// finishes is either fine (an indirect, pointer reference) or a genuine
/// cycle (a direct, by-value one) -- see `Driver::ensure_item`. Deliberately
/// item-granular, not module-granular: a foreign module's signature used to
/// be consumed as one atomic unit, but there's no longer any such unit --
/// every top-level item (same-module or cross-module, generic instantiation
/// or not) is its own independent query, so one bad item never poisons an
/// unrelated sibling's.
enum QueryState {
    InProgress,
    Done,
}

/// One `(module_path, alias)` import-alias query's memoized state -- the
/// same white/gray/black cycle guard `QueryState` already gives items,
/// applied at the same fine granularity instead of a whole module at once.
/// This granularity is what actually fixes the false-cycle bug a
/// module-wide version of this guard used to have: resolving one item's
/// `Context` used to require its *entire* module's import list resolved
/// first (every alias, whether or not that specific item's signature/body
/// even mentions it), so two modules whose *unrelated* items happened to
/// cross-import each other's module would deadlock on each other's whole
/// list -- even though the specific aliases in question never referenced
/// each other. Per-alias, only a name that genuinely, directly needs
/// itself (module A's alias `x` requiring module B's alias `y` requiring
/// module A's *same* alias `x` again) still reports `Cycle`.
enum ImportCacheState {
    InProgress,
    Done(Result<ImportTarget, ResolveError>),
}

/// Owns everything module-tree-shaped: filesystem discovery, a parsed-HIR
/// cache (each file parsed at most once), and the global, item-granular
/// query that replaces the old per-module signature cache -- see
/// `ensure_item`, the one mechanism now behind same-module resolution,
/// cross-module resolution, *and* generic instantiation
/// (`omega_analyzer::Context::resolve_type`'s unqualified-miss fallback, a
/// qualified reference, and a `Type::Generic`/generic function call all end
/// up here).
pub struct Driver {
    roots: SearchRoots,
    /// Every `--extern=<name>:<path>` the CLI was given, keyed by the name
    /// the import syntax (`import extern::<name>;`) selects it with --
    /// each is its own single-entry search root, entirely separate from
    /// `roots`. A module path whose first segment matches a key here is an
    /// *extern* module: it's resolved against that root instead of `roots`
    /// (see `search_roots_for`), and it's never eagerly signature-swept or
    /// body-checked/codegen'd by `compile` (see `is_extern_module`) -- only
    /// scanned on demand, exactly like a generic instantiation already is.
    extern_roots: HashMap<Ident, PathBuf>,
    next_module_id: u32,
    /// Counter behind every synthetic `HirId` minted for a generic
    /// instantiation's own identity (a struct instantiation's cell, or a
    /// function/method instantiation's compiled symbol) -- see
    /// `fresh_synthetic_id`. Always paired with `omega_hir::SYNTHETIC_MODULE`,
    /// a module id the lowerer never produces, so these can never collide
    /// with a real per-file `HirId`.
    next_synthetic_id: u32,
    parsed: HashMap<Vec<Ident>, Rc<HirModule>>,
    module_ids: HashMap<Vec<Ident>, ModuleId>,
    /// Whether a parsed module is *directory-shaped* (has its own
    /// `children_dir` -- see `fs_resolve::ModuleLocation`), recorded the
    /// moment it's first located in `parse_module`. This is exactly what
    /// `relative_base` needs to know "where do THIS module's own relative
    /// imports start looking": a directory-shaped module's children live
    /// directly under it (its own path *is* its relative base), while a
    /// leaf file's siblings live in its parent directory (its relative base
    /// is its path minus its last segment).
    module_shapes: HashMap<Vec<Ident>, bool>,
    /// Every parsed module's source text + on-disk path, kept for
    /// diagnostic rendering (an error snippet needs the original text long
    /// after parsing) -- see `source_file`.
    sources: HashMap<Vec<Ident>, Rc<SourceFile>>,
    /// Modules whose own file failed to parse / macro-expand, with the
    /// real, structured errors -- stashed here (rather than stuffed
    /// pre-rendered into a `ResolveError` message) because `parse_module`'s
    /// callers only speak `ResolveError`; `compile` turns these back into
    /// first-class `CompileError::Parse`/`MacroExpansion` at the end.
    parse_failures: HashMap<Vec<Ident>, Vec<ParseError>>,
    macro_failures: HashMap<Vec<Ident>, MacroError>,
    /// Every module's top-level items, indexed by name -- built once, the
    /// first time a module is touched (alongside duplicate-name detection,
    /// folded into `module_errors`); this is what `ensure_item` looks a name
    /// up in to find *what* to analyze the first time it's asked for.
    local_items: HashMap<Vec<Ident>, HashMap<Ident, usize>>,
    /// Every name in a module that names *more than one* function
    /// (`local_items` above still points only at the first-declared one,
    /// unused by the overload path) -- built alongside `local_items` in
    /// `ensure_module_indexed`. A name absent here is never overloaded:
    /// it's either not a function at all, or a plain, ordinary one-item
    /// name, both served by the unchanged `local_items`/`ensure_item` path.
    function_overloads: HashMap<Vec<Ident>, HashMap<Ident, Vec<usize>>>,
    /// One overload candidate's resolved signature, memoized by item index
    /// rather than by name (see `ensure_overload_signature`) -- unlike
    /// `struct_cells`/`enum_cells`/`ItemKey`'s `query_state`, a function
    /// signature has no self-referential-cycle risk of its own (nothing
    /// ever embeds a function *by value* the way a struct field can embed
    /// another struct), so this is a plain memoizing cache, no `InProgress`
    /// guard needed.
    overload_signatures: HashMap<(Vec<Ident>, usize), ResolvedFunctionType>,
    /// One overload candidate's fully checked body, memoized the same way
    /// (see `ensure_overload_body`) -- merged into its module's `items`
    /// during `compile`'s overload sweep, mirroring how an ordinary item's
    /// checked body is collected.
    overload_bodies: HashMap<(Vec<Ident>, usize), (CheckedItem, Vec<AnalysisWarning>)>,
    /// Every module's own `import` statements, indexed by the alias each
    /// one binds -- built once per module (alongside `local_items`'s
    /// duplicate-*name* detection, this is `ensure_module_indexed`'s
    /// duplicate-*alias* detection), and purely syntactic: computing the
    /// absolute path an import's `root`+`path` names (`Driver::
    /// import_absolute_path`) needs no signature lookup, no recursion, no
    /// filesystem access beyond what's already cached -- only *resolving*
    /// what that absolute path actually is (a module vs. an item) is
    /// deferred, lazily, to `resolve_import_alias`.
    raw_imports: HashMap<Vec<Ident>, HashMap<Ident, (HirId, Span, Vec<Ident>)>>,
    /// One `(module_path, alias)` import alias's resolved target, memoized
    /// and cycle-guarded (see `ImportCacheState`'s doc comment) at that same
    /// fine granularity -- replaces the old whole-module-granular version of
    /// this cache entirely.
    import_cache: HashMap<(Vec<Ident>, Ident), ImportCacheState>,
    /// Every struct's shared identity cell, decoupled from any one module's
    /// analysis -- created the moment *anyone* (same-module or foreign)
    /// first asks about a given struct (instantiation included), independent
    /// of whether its own module has started resolving it yet. This is what
    /// lets an indirect (pointer) reference to a struct that's mid-collection
    /// -- anywhere, same module or a different one, same instantiation or
    /// not -- be served immediately, without needing exclusive access to
    /// whatever is currently building it.
    struct_cells: HashMap<ItemKey, Rc<RefCell<ResolvedStructType>>>,
    /// The enum counterpart of `struct_cells` -- same lifecycle, same
    /// serve-while-`InProgress` role (a variant body's `*MyEnum` pointer
    /// back at the enum still being collected resolves through this).
    enum_cells: HashMap<ItemKey, Rc<RefCell<ResolvedEnumType>>>,
    /// The union counterpart of `struct_cells` -- same lifecycle, same role.
    union_cells: HashMap<ItemKey, Rc<RefCell<ResolvedUnionType>>>,
    query_state: HashMap<ItemKey, QueryState>,
    /// Every item that finished its query successfully -- absent for one
    /// that's `Done` but failed (see `ensure_item`); the real diagnostics
    /// for those live in `module_errors` instead.
    resolved_items: HashMap<ItemKey, (Visibility, ResolvedItem)>,
    /// Every generic instantiation's fully checked body, discovered and
    /// computed on demand (see `ensure_item`'s trigger right after a fresh
    /// instantiation's signature succeeds) rather than via `compile`'s
    /// static per-module sweep, since instantiations aren't statically
    /// enumerable up front. Merged into their originating module's
    /// `CheckedModule.items` only after `compile`'s whole two-phase sweep
    /// finishes (see `compile`'s final assembly step) -- an instantiation
    /// can be discovered at any point during either phase, including after
    /// its originating module's own ordinary items have already been
    /// collected, so nothing may assume this map is complete any earlier.
    generic_instantiations: HashMap<ItemKey, (CheckedItem, Vec<AnalysisWarning>)>,
    /// Every `AnalysisError` produced so far, keyed by the module it belongs
    /// to -- accumulated across both the signature phase (`ensure_item`) and
    /// the body phase (`compile`'s second pass), since neither one is a
    /// single long-lived whole-module `Analyzer` pass anymore.
    module_errors: HashMap<Vec<Ident>, Vec<AnalysisError>>,
}

impl Driver {
    pub fn new(roots: Vec<PathBuf>, extern_roots: HashMap<Ident, PathBuf>) -> Self {
        Self {
            roots: SearchRoots(roots),
            extern_roots,
            next_module_id: 0,
            next_synthetic_id: 0,
            parsed: HashMap::new(),
            module_ids: HashMap::new(),
            module_shapes: HashMap::new(),
            sources: HashMap::new(),
            parse_failures: HashMap::new(),
            macro_failures: HashMap::new(),
            local_items: HashMap::new(),
            function_overloads: HashMap::new(),
            overload_signatures: HashMap::new(),
            overload_bodies: HashMap::new(),
            raw_imports: HashMap::new(),
            import_cache: HashMap::new(),
            struct_cells: HashMap::new(),
            enum_cells: HashMap::new(),
            union_cells: HashMap::new(),
            query_state: HashMap::new(),
            resolved_items: HashMap::new(),
            generic_instantiations: HashMap::new(),
            module_errors: HashMap::new(),
        }
    }

    fn fresh_module_id(&mut self) -> ModuleId {
        let id = ModuleId(self.next_module_id);
        self.next_module_id += 1;
        id
    }

    /// Whether `path` names an *extern* module -- a pure function of its own
    /// first segment, no separate bookkeeping needed: `import
    /// extern::<name>::...` always makes `<name>` the resulting absolute
    /// path's own first segment (see `import_absolute_path`), and every
    /// module reachable *from* an extern module keeps that same segment
    /// leading (relative/root-rooted imports only ever extend a path, never
    /// replace its existing prefix) -- so this single check is correct
    /// everywhere, not just for the module an `extern::` import named
    /// directly.
    fn is_extern_module(&self, path: &[Ident]) -> bool {
        path.first().is_some_and(|head| self.extern_roots.contains_key(head))
    }

    /// Which filesystem root(s) to search for `path`: its own registered
    /// extern root (see `is_extern_module`) if it's an extern module, else
    /// the local project's own `roots`. Every `locate_module` call site goes
    /// through this instead of reading `self.roots` directly.
    fn search_roots_for(&self, path: &[Ident]) -> Vec<PathBuf> {
        if let Some(head) = path.first()
            && let Some(root) = self.extern_roots.get(head)
        {
            return vec![root.clone()];
        }
        self.roots.0.clone()
    }

    /// Where `module_path`'s own *unrooted* (`ImportRoot::Local`) imports
    /// start looking -- see `module_shapes`'s doc comment for the rule.
    /// Always called with an already-parsed module (an import can only be
    /// resolved for a module whose own `HirItem::Import` list is already in
    /// hand), so `module_shapes` is guaranteed populated.
    fn relative_base(&self, module_path: &[Ident]) -> Vec<Ident> {
        if self.module_shapes.get(module_path).copied().unwrap_or(false) {
            module_path.to_vec()
        } else {
            module_path[..module_path.len().saturating_sub(1)].to_vec()
        }
    }

    /// The absolute module path one `import` statement's `root`+`path`
    /// names, given the *importing* module's own path -- pure path
    /// arithmetic (no recursive item resolution, no filesystem access
    /// beyond what `relative_base` already cached), mirroring
    /// `reachable_target`'s existing cheap, non-recursive nature. See
    /// `ImportRoot`'s own doc comment for what each variant means.
    fn import_absolute_path(
        &self,
        importer: &[Ident],
        root: ImportRoot,
        path: &Path,
    ) -> Result<Vec<Ident>, ResolveError> {
        match root {
            ImportRoot::Local => {
                let mut absolute = self.relative_base(importer);
                absolute.extend(path.segments());
                Ok(absolute)
            }
            // "Root of *my* current project" -- if the importer is itself
            // part of an extern project (its own path leads with that
            // project's alias), re-prepend that same alias so the result
            // stays correctly anchored to *that* project's root rather than
            // silently falling back to the local one (see `is_extern_module`'s
            // doc comment on why every module reachable from an extern one
            // must keep its alias leading).
            ImportRoot::ProjectRoot => {
                let mut absolute = Vec::new();
                if importer.first().is_some_and(|head| self.extern_roots.contains_key(head)) {
                    absolute.push(importer[0].clone());
                }
                absolute.extend(path.segments());
                Ok(absolute)
            }
            // `path.head` *is* the extern alias, by convention -- checked
            // eagerly here (rather than left to `locate_module`'s ordinary
            // not-found handling) so a typo'd or forgotten `--extern` flag
            // gets its own precise diagnostic instead of a generic "module
            // not found".
            ImportRoot::Extern => {
                if !self.extern_roots.contains_key(&path.head) {
                    return Err(ResolveError::UnknownExtern(path.head.clone()));
                }
                Ok(path.segments())
            }
        }
    }

    /// A fresh, globally unique `HirId` for a generic instantiation's own
    /// identity -- never produced by the lowerer (see `SYNTHETIC_MODULE`),
    /// so it can never collide with a real per-file one. Minted exactly
    /// once per instantiated struct/function/method, inside `compute_item`
    /// (the single place instantiation identity is decided -- see its doc
    /// comment) -- everywhere else reads it back rather than minting again.
    fn fresh_synthetic_id(&mut self) -> HirId {
        let id = HirId { module: SYNTHETIC_MODULE, local: self.next_synthetic_id };
        self.next_synthetic_id += 1;
        id
    }

    /// Parses (and lowers) `path`'s own file, memoized -- the mechanism
    /// behind "only resolve things that are imported" (a module is only
    /// ever parsed on demand, the first time something actually needs it)
    /// and "avoid reanalyzing a file over and over" (every subsequent
    /// request is a cache hit). A directory-shaped module with no own file
    /// (a pure namespace, e.g. `mymodule/` with no `mymodule/mymodule.omg`)
    /// is a valid, empty module -- not an error.
    fn parse_module(&mut self, path: &[Ident]) -> Result<Rc<HirModule>, ResolveError> {
        if let Some(hir) = self.parsed.get(path) {
            return Ok(hir.clone());
        }

        let location = locate_module(&self.search_roots_for(path), path)?;
        self.module_shapes.insert(path.to_vec(), location.children_dir.is_some());
        let module_id = self.fresh_module_id();

        let hir = match location.own_file {
            None => HirModule { id: module_id, items: vec![] },
            Some(file) => {
                let source = std::fs::read_to_string(&file).map_err(|e| ResolveError::LoadFailed {
                    path: path.to_vec(),
                    message: e.to_string(),
                })?;
                self.sources
                    .insert(path.to_vec(), Rc::new(SourceFile::new(file.display().to_string(), source.as_str())));
                // Parse/macro failures stash their real, structured errors
                // (see `parse_failures`) and return a `LoadFailed` whose
                // message is only a fallback -- `compile` recognizes the
                // stash and reports the structured form instead.
                let ast = SourceModule::parse(&source).map_err(|errors| {
                    self.parse_failures.insert(path.to_vec(), errors);
                    ResolveError::LoadFailed { path: path.to_vec(), message: "the module has syntax errors".into() }
                })?;
                let ast = omega_parser::macros::expand(ast).map_err(|e| {
                    self.macro_failures.insert(path.to_vec(), e);
                    ResolveError::LoadFailed { path: path.to_vec(), message: "macro expansion failed".into() }
                })?;
                omega_hir::lower_module(module_id, &ast)
            }
        };

        let hir = Rc::new(hir);
        self.parsed.insert(path.to_vec(), hir.clone());
        self.module_ids.insert(path.to_vec(), module_id);
        Ok(hir)
    }

    /// Determines which module path must become reachable for one `import`
    /// statement -- itself, if it names a real module (a whole-module
    /// import), otherwise its parent (an item import: only the item's
    /// *owning* module needs to be parsed, never a same-named own-file it
    /// doesn't need). Same undecidable-from-syntax-alone disambiguation
    /// `resolve_import` does at analysis time, but cheaper -- a filesystem
    /// check is all "which file(s) must be parsed" needs; it doesn't
    /// require a signature lookup the way "what does this name resolve to"
    /// does. `importer` is the module the `import` statement itself lives
    /// in, needed to make sense of `Local`/`ProjectRoot`-rooted paths (see
    /// `import_absolute_path`).
    fn reachable_target(&self, importer: &[Ident], import: &HirImport) -> Result<Vec<Ident>, ResolveError> {
        let segments = self.import_absolute_path(importer, import.root, &import.path)?;
        match locate_module(&self.search_roots_for(&segments), &segments) {
            Ok(_) => return Ok(segments),
            // A structural problem with `segments` itself (two filesystem
            // entries claiming the same name) -- real regardless of whether
            // this turns out to be a whole-module or an item import, so it
            // must surface here rather than being masked by the parent-path
            // fallback below (which would otherwise wrongly report this as
            // a plain "unknown module").
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }
        if segments.len() > 1 {
            let parent = &segments[..segments.len() - 1];
            if locate_module(&self.search_roots_for(parent), parent).is_ok() {
                return Ok(parent.to_vec());
            }
        }
        Err(ResolveError::UnknownModule(segments))
    }

    /// The reachable module set: a worklist over raw `import` paths starting
    /// at `entry`, parsing each module exactly once as it's discovered.
    /// Nothing outside this set is ever parsed or analyzed -- the whole
    /// point of resolving imports lazily rather than eagerly walking the
    /// entire search tree.
    ///
    /// Failures come back as first-class `CompileError`s, each tagged with
    /// the `import` statement that pulled the failing module in (the
    /// worklist carries every entry's importing site along) -- so "cannot
    /// find module" points at the actual `import` line, and a module with
    /// syntax errors reports those errors themselves rather than a
    /// second-hand summary.
    fn discover_reachable(&mut self, entry: &[Ident]) -> Result<Vec<Vec<Ident>>, CompileError> {
        type Importer = Option<(Vec<Ident>, Span)>;
        let mut reachable = vec![entry.to_vec()];
        let mut worklist: Vec<(Vec<Ident>, Importer)> = vec![(entry.to_vec(), None)];
        let mut seen: std::collections::HashSet<Vec<Ident>> = std::collections::HashSet::from([entry.to_vec()]);

        while let Some((path, importer)) = worklist.pop() {
            let hir = match self.parse_module(&path) {
                Ok(hir) => hir,
                Err(error) => return Err(self.load_failure(&path, error, importer)),
            };
            for item in &hir.items {
                let HirItem::Import(import) = item else { continue };
                let target = match self.reachable_target(&path, import) {
                    Ok(target) => target,
                    Err(error) => {
                        return Err(CompileError::Resolve { error, importer: Some((path.clone(), import.span)) });
                    }
                };
                if seen.insert(target.clone()) {
                    reachable.push(target.clone());
                    worklist.push((target, Some((path.clone(), import.span))));
                }
            }
        }

        Ok(reachable)
    }

    /// Turns a module-load failure into its first-class `CompileError`:
    /// the stashed parse/macro-expansion errors when that's what actually
    /// went wrong (see `parse_failures`), or the resolve error itself,
    /// tagged with the importing site, otherwise.
    fn load_failure(
        &mut self,
        module: &[Ident],
        error: ResolveError,
        importer: Option<(Vec<Ident>, Span)>,
    ) -> CompileError {
        if let Some(errors) = self.parse_failures.remove(module) {
            return CompileError::Parse { module: module.to_vec(), errors };
        }
        if let Some(macro_error) = self.macro_failures.remove(module) {
            return CompileError::MacroExpansion { module: module.to_vec(), error: macro_error };
        }
        CompileError::Resolve { error, importer }
    }

    /// The parsed source of `module`, for rendering its diagnostics --
    /// present for every module that got as far as being read off disk.
    pub fn source_file(&self, module: &[Ident]) -> Option<Rc<SourceFile>> {
        self.sources.get(module).cloned()
    }

    /// Builds (once, cached in `local_items`) module `path`'s top-level item
    /// index, recording a `Redeclaration` error in `module_errors` for each
    /// duplicate name -- what used to happen once per module inside
    /// `Analyzer::new`, back when one `Analyzer` handled a whole module
    /// rather than one item.
    fn ensure_module_indexed(&mut self, path: &[Ident]) -> Result<(), ResolveError> {
        if self.local_items.contains_key(path) {
            return Ok(());
        }
        let hir = self.parse_module(path)?;
        let mut index = HashMap::new();
        let mut overloads: HashMap<Ident, Vec<usize>> = HashMap::new();
        for (i, item) in hir.items.iter().enumerate() {
            let Some(name) = item_name(item) else { continue };
            let is_function = matches!(item, HirItem::FunctionDefinition(_));
            match index.entry(name.clone()) {
                Entry::Occupied(first) => {
                    let first_index = *first.get();
                    let first_is_function = matches!(&hir.items[first_index], HirItem::FunctionDefinition(_));
                    if is_function && first_is_function {
                        // A valid overload *candidate* -- not a
                        // redeclaration (see `function_overloads`'s doc
                        // comment). Whether it's genuinely distinct (a
                        // different signature) is checked later, once every
                        // candidate's signature is actually resolved (see
                        // `check_overload_duplicates`) -- nothing here has
                        // access to param types yet.
                        overloads.entry(name).or_insert_with(|| vec![first_index]).push(i);
                    } else {
                        let (id, span) = item_id_span(item);
                        let (_, previous) = item_id_span(&hir.items[first_index]);
                        self.module_errors.entry(path.to_vec()).or_default().push(AnalysisError::new(
                            id,
                            span,
                            AnalysisErrorKind::Redeclaration { name, previous: Some(previous) },
                        ));
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(i);
                }
            }
        }
        self.local_items.insert(path.to_vec(), index);
        self.function_overloads.insert(path.to_vec(), overloads);

        // The raw import-alias index: purely syntactic (see `raw_imports`'s
        // doc comment), so this stays cheap and resolution-free even though
        // it runs eagerly for every module the moment it's indexed --
        // there's no cycle risk here, only in actually *resolving* what
        // each alias's absolute path names (`resolve_import_alias`).
        let mut aliases: HashMap<Ident, (HirId, Span, Vec<Ident>)> = HashMap::new();
        for item in &hir.items {
            let HirItem::Import(import) = item else { continue };
            let alias = import.path.tail.last().cloned().unwrap_or_else(|| import.path.head.clone());
            let absolute = match self.import_absolute_path(path, import.root, &import.path) {
                Ok(absolute) => absolute,
                Err(e) => {
                    self.module_errors.entry(path.to_vec()).or_default().push(AnalysisError::new(
                        import.id,
                        import.span,
                        AnalysisErrorKind::ModuleResolution(e),
                    ));
                    continue;
                }
            };
            match aliases.entry(alias) {
                Entry::Occupied(existing) => {
                    let (_, previous_span, _) = *existing.get();
                    self.module_errors.entry(path.to_vec()).or_default().push(AnalysisError::new(
                        import.id,
                        import.span,
                        AnalysisErrorKind::Redeclaration { name: existing.key().clone(), previous: Some(previous_span) },
                    ));
                }
                Entry::Vacant(entry) => {
                    entry.insert((import.id, import.span, absolute));
                }
            }
        }
        self.raw_imports.insert(path.to_vec(), aliases);

        Ok(())
    }

    /// Module `path`'s item `name`'s position in its own `HirModule::items`
    /// -- indexes the module first if needed. Shared by `raw_item_generics`
    /// and `ensure_item`'s own dispatch, so "index the module, look the name
    /// up, report `UnknownItem` if absent" is only written once.
    fn local_item_index(&mut self, module_path: &[Ident], name: &Ident) -> Result<usize, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        self.local_items
            .get(module_path)
            .and_then(|idx| idx.get(name))
            .copied()
            .ok_or_else(|| ResolveError::UnknownItem { module: module_path.to_vec(), item: name.clone() })
    }

    /// The item's own declared generic parameter names (empty = non-generic),
    /// with no analysis or instantiation triggered -- just a HIR field read
    /// behind the module index. The single source of truth for every "is
    /// this generic" check: `resolve_import`'s item-case (a generic item
    /// import supplies no type arguments, so it must not eagerly instantiate
    /// via `ensure_item`), `compile`'s phase-1/phase-2 sweeps (which must
    /// skip an uninstantiated template rather than fail it with a spurious
    /// arg-count mismatch), and `ensure_item`'s own arg-count validation.
    fn raw_item_generics(&mut self, module_path: &[Ident], name: &Ident) -> Result<Vec<Ident>, ResolveError> {
        let index = self.local_item_index(module_path, name)?;
        let hir = self.parsed.get(module_path).expect("parsed by local_item_index");
        Ok(match &hir.items[index] {
            HirItem::Struct(s) => s.generics.clone(),
            HirItem::Enum(e) => e.generics.clone(),
            HirItem::Union(u) => u.generics.clone(),
            HirItem::FunctionDefinition(f) => f.generics.clone(),
            HirItem::Declaration(_) | HirItem::ExternDeclaration(_) => vec![],
            HirItem::Import(_) => unreachable!("imports are never indexed into local_items"),
        })
    }

    /// `alias`'s resolved target in `module_path`, given its already-raw-
    /// indexed absolute path (`raw_imports`) -- cycle-guarded per
    /// `(module_path, alias)` pair (see `ImportCacheState`'s doc comment).
    /// The `ModuleResolver::resolve_import_alias` trait method (`impl
    /// ModuleResolver for Driver`, below) is a thin wrapper around this that
    /// also handles "not an alias at all" (`Ok(None)`, no `raw_imports`
    /// entry -- never enters the cache, since there's nothing to resolve).
    fn resolve_import_alias_cached(&mut self, module_path: &[Ident], alias: &Ident, absolute: &[Ident]) -> Result<ImportTarget, ResolveError> {
        let key = (module_path.to_vec(), alias.clone());
        match self.import_cache.get(&key) {
            Some(ImportCacheState::Done(result)) => return result.clone(),
            Some(ImportCacheState::InProgress) => return Err(ResolveError::Cycle(vec![module_path.to_vec()])),
            None => {}
        }

        self.import_cache.insert(key.clone(), ImportCacheState::InProgress);
        let result = self.resolve_absolute_import_target(absolute);
        self.import_cache.insert(key, ImportCacheState::Done(result.clone()));
        result
    }

    /// What an already-absolute path names -- a real module (a pure
    /// filesystem check, no recursion at all), a generic item (deferred,
    /// see `ImportTarget::GenericItem`'s doc comment), or an ordinary item
    /// (eagerly resolved via `ensure_item`). Exactly `resolve_import`'s old
    /// body, just taking an already-computed absolute path instead of
    /// re-deriving `segments` from a raw `Path` itself.
    fn resolve_absolute_import_target(&mut self, segments: &[Ident]) -> Result<ImportTarget, ResolveError> {
        match locate_module(&self.search_roots_for(segments), segments) {
            Ok(_) => return Ok(ImportTarget::Module(segments.to_vec())),
            // Real regardless of whether this turns out to be a
            // whole-module or item import -- must surface here, not be
            // masked by the item-import fallback below (see
            // `Driver::reachable_target`'s identical fix).
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }

        let Some((item_name, module_path)) = segments.split_last() else {
            return Err(ResolveError::UnknownModule(segments.to_vec()));
        };

        // A *generic* item import supplies no type arguments at all (those
        // only ever appear at a use site) -- eagerly instantiating via
        // `ensure_item` here would always fail with a spurious arg-count
        // mismatch, so this defers entirely, carrying just the absolute
        // path for `Context::generic_aliases` to substitute in later.
        if !self.raw_item_generics(module_path, item_name)?.is_empty() {
            return Ok(ImportTarget::GenericItem(segments.to_vec()));
        }

        // Capturing "what does this alias refer to" never embeds anything
        // inline the way a struct field does -- always indirect.
        Ok(ImportTarget::Item(self.ensure_item(module_path, item_name, &[], true)?))
    }

    /// Gets (or creates) `key`'s shared identity cell -- see `struct_cells`'s
    /// doc comment. Always called with a real `id` (the struct's own
    /// `HirId`, or a freshly minted synthetic one for an instantiation) the
    /// first time, from `compute_item`, right before this same struct is
    /// marked `InProgress` and analyzed, so nothing can observe a missing
    /// cell for a struct that's actually `InProgress` (see `ensure_item`'s
    /// indirect+in-progress branch).
    fn struct_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedStructType>> {
        self.struct_cells
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedStructType { id, name: key.1.clone(), fields: vec![], functions: vec![] }))
            })
            .clone()
    }

    /// The enum counterpart of `struct_cell` -- same creation contract (see
    /// its doc comment). The placeholder's tag defaults to the implicit
    /// `u16`; `signature_of_enum` patches the real shape in.
    fn enum_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedEnumType>> {
        self.enum_cells
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedEnumType {
                    id,
                    name: key.1.clone(),
                    tag_type: ResolvedType::U16,
                    header: vec![],
                    variants: vec![],
                    functions: vec![],
                }))
            })
            .clone()
    }

    /// The union counterpart of `struct_cell` -- same creation contract (see
    /// its doc comment).
    fn union_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedUnionType>> {
        self.union_cells
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedUnionType { id, name: key.1.clone(), fields: vec![], functions: vec![] }))
            })
            .clone()
    }

    /// The one global query behind same-module resolution, cross-module
    /// resolution, and generic instantiation alike -- see
    /// `ModuleResolver::resolve_item`'s doc comment. A name already `Done`
    /// is a cache hit (successful or not); one found `InProgress` is either
    /// a legitimate indirect (pointer) reference to something still being
    /// built (served straight from `struct_cells`) or a genuine by-value
    /// cycle (`RecursiveTypeWithoutIndirection`); anything else is analyzed
    /// right here, on the spot, before this returns -- and, for a *fresh*
    /// generic instantiation, its body is checked immediately afterward too
    /// (see the trigger at the end of this method).
    pub fn ensure_item(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        type_args: &[ResolvedType],
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let key: ItemKey = (module_path.to_vec(), name.clone(), type_args.to_vec());

        match self.query_state.get(&key) {
            Some(QueryState::Done) => {
                let Some((visibility, item)) = self.resolved_items.get(&key) else {
                    return Err(ResolveError::ItemFailed { module: module_path.to_vec(), item: name.clone() });
                };
                return match visibility {
                    Visibility::Public => Ok(item.clone()),
                };
            }
            Some(QueryState::InProgress) => {
                if indirect {
                    // Only a *type* reference to a struct can legitimately
                    // stay indirect forever (a pointer never needs its
                    // pointee's own layout) -- if `key` has no cell, this
                    // isn't that: it's an *import* (always `indirect = true`,
                    // regardless of what it names) looping back on an item
                    // that's still being computed, i.e. a genuine mutual
                    // item-style import cycle (`import b::x;` in one module,
                    // `import a::y;` in the other) reaching back around
                    // through a *different* path than `imports`'s own guard
                    // covers -- a real cycle, cleanly rejected here rather
                    // than assumed impossible.
                    if let Some(cell) = self.struct_cells.get(&key) {
                        return Ok(ResolvedItem::Type(ResolvedType::Struct(cell.clone())));
                    }
                    if let Some(cell) = self.enum_cells.get(&key) {
                        return Ok(ResolvedItem::Type(ResolvedType::Enum { cell: cell.clone(), variant: None }));
                    }
                    if let Some(cell) = self.union_cells.get(&key) {
                        return Ok(ResolvedItem::Type(ResolvedType::Union(cell.clone())));
                    }
                    return Err(ResolveError::Cycle(vec![module_path.to_vec()]));
                }
                return Err(ResolveError::RecursiveTypeWithoutIndirection {
                    module: module_path.to_vec(),
                    item: name.clone(),
                });
            }
            None => {}
        }

        let index = self.local_item_index(module_path, name)?;
        let generics = self.raw_item_generics(module_path, name)?;
        if generics.len() != type_args.len() {
            return Err(ResolveError::GenericArgCountMismatch {
                module: module_path.to_vec(),
                item: name.clone(),
                expected: generics.len(),
                found: type_args.len(),
            });
        }

        self.query_state.insert(key.clone(), QueryState::InProgress);
        let result = self.compute_item(module_path, name, index, type_args, &generics);
        self.query_state.insert(key.clone(), QueryState::Done);
        if let Ok(item) = &result {
            self.resolved_items.insert(key.clone(), (Visibility::Public, item.clone()));
        }

        // A genuine instantiation's body is checked on demand, right here,
        // immediately after its own signature is marked `Done` (not while
        // it's still `InProgress`) -- this ordering is exactly why an
        // ordinary same-module recursive call doesn't hit the `InProgress`
        // branch above (its own signature is always `Done` before its body
        // is ever checked); triggering the body-check here, at this exact
        // point, preserves that same invariant for a recursive generic call
        // too, instead of only checking generic instantiations' bodies via
        // `compile`'s static per-module sweep, which can't enumerate them
        // (they aren't statically known items).
        if result.is_ok() && !type_args.is_empty() {
            self.check_generic_instantiation_body(module_path, name, type_args, index);
        }

        result
    }

    /// Does the actual work `ensure_item` defers to the first time a name is
    /// requested: builds one throwaway `Analyzer` for this one item (seeded
    /// with the module's already-resolved imports and, for a generic
    /// instantiation, its concrete substitution), dispatches by item kind,
    /// and folds whatever errors it produced into `module_errors`. A
    /// struct's cell is fetched/created *before* the `Analyzer` runs, so a
    /// self- or mutually-referencing pointer field hit during field
    /// resolution finds it already there (`ensure_item`'s `InProgress`
    /// branch serves it).
    ///
    /// **Identity is decided exactly once, here, for a fresh key, and never
    /// again**: `id` (`ResolvedStructType.id`/`ResolvedItem::Value.decl_id`)
    /// is the item's own `HirId` for a non-generic call (`type_args` empty,
    /// behavior-preserving), or a freshly minted synthetic one for a genuine
    /// instantiation -- both `struct_cell` and `check_item_body` read this
    /// same decided id back out of `resolved_items`/the cell afterward
    /// rather than ever recomputing it, so `List<u32>` and `List<i64>` are
    /// guaranteed genuinely distinct types/symbols with no risk of drift
    /// between the signature and body phases.
    fn compute_item(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        index: usize,
        type_args: &[ResolvedType],
        generics: &[Ident],
    ) -> Result<ResolvedItem, ResolveError> {
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        let item = &hir.items[index];
        let substitution: Vec<(Ident, ResolvedType)> =
            generics.iter().cloned().zip(type_args.iter().cloned()).collect();

        let (result, errors) = match item {
            HirItem::Declaration(decl) => {
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (decl.id, decl.span));
                let checked = analyzer.analyze_declaration(decl, Storage::Global);
                let (errors, _warnings) = analyzer.finish();
                let result = checked
                    .map(|c| ResolvedItem::Value { r#type: c.r#type, storage: Storage::Global, decl_id: c.id });
                (result, errors)
            }
            HirItem::ExternDeclaration(decl) => {
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (decl.id, decl.span));
                let checked = analyzer.analyze_extern_decl(decl);
                let (errors, _warnings) = analyzer.finish();
                let result = checked.map(|c| {
                    let storage =
                        if matches!(c.r#type, ResolvedType::Function(_)) { Storage::Function } else { Storage::Global };
                    ResolvedItem::Value { r#type: c.r#type, storage, decl_id: c.id }
                });
                (result, errors)
            }
            HirItem::FunctionDefinition(f) => {
                let id = if type_args.is_empty() { f.id } else { self.fresh_synthetic_id() };
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (f.id, f.span));
                let checked = analyzer.collect_function_signature(f);
                let (errors, _warnings) = analyzer.finish();
                let result = checked.map(|fn_type| ResolvedItem::Value {
                    r#type: ResolvedType::Function(fn_type),
                    storage: Storage::Function,
                    decl_id: id,
                });
                (result, errors)
            }
            HirItem::Struct(s) => {
                let id = if type_args.is_empty() { s.id } else { self.fresh_synthetic_id() };
                let key: ItemKey = (module_path.to_vec(), name.clone(), type_args.to_vec());
                let cell = self.struct_cell(&key, id);
                let method_ids: Vec<HirId> = s
                    .functions
                    .iter()
                    .map(|f| if type_args.is_empty() { f.id } else { self.fresh_synthetic_id() })
                    .collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (s.id, s.span));
                let ok = analyzer.signature_of_struct(s, &cell, &method_ids);
                let (errors, _warnings) = analyzer.finish();
                let result = ok.map(|()| ResolvedItem::Type(ResolvedType::Struct(cell)));
                (result, errors)
            }
            HirItem::Enum(e) => {
                let id = if type_args.is_empty() { e.id } else { self.fresh_synthetic_id() };
                let key: ItemKey = (module_path.to_vec(), name.clone(), type_args.to_vec());
                let cell = self.enum_cell(&key, id);
                let method_ids: Vec<HirId> = e
                    .functions
                    .iter()
                    .map(|f| if type_args.is_empty() { f.id } else { self.fresh_synthetic_id() })
                    .collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (e.id, e.span));
                let ok = analyzer.signature_of_enum(e, &cell, &method_ids);
                let (errors, _warnings) = analyzer.finish();
                let result = ok.map(|()| ResolvedItem::Type(ResolvedType::Enum { cell, variant: None }));
                (result, errors)
            }
            HirItem::Union(u) => {
                let id = if type_args.is_empty() { u.id } else { self.fresh_synthetic_id() };
                let key: ItemKey = (module_path.to_vec(), name.clone(), type_args.to_vec());
                let cell = self.union_cell(&key, id);
                let method_ids: Vec<HirId> = u
                    .functions
                    .iter()
                    .map(|f| if type_args.is_empty() { f.id } else { self.fresh_synthetic_id() })
                    .collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (u.id, u.span));
                let ok = analyzer.signature_of_union(u, &cell, &method_ids);
                let (errors, _warnings) = analyzer.finish();
                let result = ok.map(|()| ResolvedItem::Type(ResolvedType::Union(cell)));
                (result, errors)
            }
            HirItem::Import(_) => unreachable!("imports are never indexed into local_items"),
        };

        if !errors.is_empty() {
            self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
        }

        result.ok_or_else(|| ResolveError::ItemFailed { module: module_path.to_vec(), item: name.clone() })
    }

    /// Checks one item's *body* (phase 2 -- see `compile`), reading its
    /// already-`Done` signature straight out of `resolved_items`/
    /// `struct_cells` rather than re-resolving it. `Declaration`/
    /// `ExternDeclaration` have no body of their own, so no `Analyzer` call
    /// is needed for them at all -- just their already-resolved type, paired
    /// with the identifying fields already sitting on the `HirItem`. Used
    /// both by `compile`'s static per-module sweep (`type_args` always
    /// empty there) and `check_generic_instantiation_body`'s on-demand
    /// trigger (a real substitution) -- one mechanism for both.
    fn check_item_body(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        item: &HirItem,
        type_args: &[ResolvedType],
    ) -> Option<(CheckedItem, Vec<AnalysisWarning>)> {
        let key: ItemKey = (module_path.to_vec(), name.clone(), type_args.to_vec());
        match item {
            HirItem::Declaration(decl) => {
                let (_, resolved) = self.resolved_items.get(&key).expect("resolved in phase 1").clone();
                let ResolvedItem::Value { r#type, .. } = resolved else {
                    unreachable!("a declaration's own resolved item is always ResolvedItem::Value");
                };
                let checked = CheckedItem::Declaration(omega_analyzer::checked::CheckedDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                });
                Some((checked, vec![]))
            }
            HirItem::ExternDeclaration(decl) => {
                let (_, resolved) = self.resolved_items.get(&key).expect("resolved in phase 1").clone();
                let ResolvedItem::Value { r#type, .. } = resolved else {
                    unreachable!("an extern's own resolved item is always ResolvedItem::Value");
                };
                let checked = CheckedItem::ExternDeclaration(omega_analyzer::checked::CheckedExternDeclaration {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                });
                Some((checked, vec![]))
            }
            HirItem::FunctionDefinition(f) => {
                let (_, resolved) = self.resolved_items.get(&key).expect("resolved in phase 1").clone();
                let ResolvedItem::Value { r#type: ResolvedType::Function(fn_type), decl_id, .. } = resolved else {
                    unreachable!("a function's own resolved item is always ResolvedType::Function");
                };
                let substitution: Vec<(Ident, ResolvedType)> =
                    f.generics.iter().cloned().zip(type_args.iter().cloned()).collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (f.id, f.span));
                let checked = analyzer.check_function_body(f, &fn_type, decl_id);
                let (errors, warnings) = analyzer.finish();
                if !errors.is_empty() {
                    self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
                }
                checked.map(|c| (CheckedItem::FunctionDefinition(c), warnings))
            }
            HirItem::Struct(s) => {
                let cell = self.struct_cells.get(&key).expect("resolved in phase 1").clone();
                let substitution: Vec<(Ident, ResolvedType)> =
                    s.generics.iter().cloned().zip(type_args.iter().cloned()).collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (s.id, s.span));
                let checked = analyzer.check_struct_body(s, &cell);
                let (errors, warnings) = analyzer.finish();
                if !errors.is_empty() {
                    self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
                }
                checked.map(|c| (CheckedItem::Struct(c), warnings))
            }
            HirItem::Enum(e) => {
                let cell = self.enum_cells.get(&key).expect("resolved in phase 1").clone();
                let substitution: Vec<(Ident, ResolvedType)> =
                    e.generics.iter().cloned().zip(type_args.iter().cloned()).collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (e.id, e.span));
                let checked = analyzer.check_enum_body(e, &cell);
                let (errors, warnings) = analyzer.finish();
                if !errors.is_empty() {
                    self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
                }
                checked.map(|c| (CheckedItem::Enum(c), warnings))
            }
            HirItem::Union(u) => {
                let cell = self.union_cells.get(&key).expect("resolved in phase 1").clone();
                let substitution: Vec<(Ident, ResolvedType)> =
                    u.generics.iter().cloned().zip(type_args.iter().cloned()).collect();
                let mut analyzer =
                    Analyzer::new(self, module_path.to_vec(), &substitution, (u.id, u.span));
                let checked = analyzer.check_union_body(u, &cell);
                let (errors, warnings) = analyzer.finish();
                if !errors.is_empty() {
                    self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
                }
                checked.map(|c| (CheckedItem::Union(c), warnings))
            }
            HirItem::Import(_) => unreachable!("imports are filtered out before this is called"),
        }
    }

    /// Body-checks a *specific* generic instantiation the moment its own
    /// signature just finished (triggered from `ensure_item`, right after
    /// marking this key `Done`) -- see `ensure_item`'s doc comment for why
    /// this ordering matters. Reuses `check_item_body` verbatim; the only
    /// difference from the ordinary per-module sweep is *when* this runs
    /// (on demand here, instead of during `compile`'s static loop, which has
    /// no way to enumerate instantiations up front) and *where the result
    /// goes* (`generic_instantiations`, merged into the right module during
    /// `compile`'s final assembly step, instead of directly into a
    /// `Vec<CheckedItem>` being built in sequence).
    fn check_generic_instantiation_body(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        type_args: &[ResolvedType],
        index: usize,
    ) {
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        let item = &hir.items[index];
        if let Some((checked, warnings)) = self.check_item_body(module_path, name, item, type_args) {
            let key: ItemKey = (module_path.to_vec(), name.clone(), type_args.to_vec());
            self.generic_instantiations.insert(key, (checked, warnings));
        }
    }

    /// One overload candidate's resolved signature (see
    /// `overload_signatures`'s doc comment), memoized by `(module_path,
    /// index)` rather than by name -- the whole reason this exists
    /// separately from `compute_item`'s identical-looking
    /// `HirItem::FunctionDefinition` branch, which is keyed by name and so
    /// can only ever address the first-declared candidate. Always
    /// non-generic (every candidate in an overload group is confirmed a
    /// plain function by `ensure_module_indexed`), so there's no
    /// `type_args`/synthetic-id decision to make the way `compute_item`
    /// has for a generic instantiation.
    fn ensure_overload_signature(
        &mut self,
        module_path: &[Ident],
        index: usize,
    ) -> Result<ResolvedFunctionType, ResolveError> {
        let key = (module_path.to_vec(), index);
        if let Some(fn_type) = self.overload_signatures.get(&key) {
            return Ok(fn_type.clone());
        }
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index ensure_module_indexed confirmed is a function");
        };
        let mut analyzer = Analyzer::new(self, module_path.to_vec(), &[], (f.id, f.span));
        let checked = analyzer.collect_function_signature(f);
        let (errors, _warnings) = analyzer.finish();
        if !errors.is_empty() {
            self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
        }
        let fn_type =
            checked.ok_or_else(|| ResolveError::ItemFailed { module: module_path.to_vec(), item: f.name.clone() })?;
        self.overload_signatures.insert(key, fn_type.clone());
        Ok(fn_type)
    }

    /// One overload candidate's fully checked body (see `overload_bodies`'s
    /// doc comment), memoized the same way. Reads its own already-resolved
    /// signature back from `ensure_overload_signature` rather than
    /// recomputing it, mirroring `check_item_body`'s identical contract for
    /// an ordinary item.
    fn ensure_overload_body(&mut self, module_path: &[Ident], index: usize) -> Option<(CheckedItem, Vec<AnalysisWarning>)> {
        let key = (module_path.to_vec(), index);
        if let Some(result) = self.overload_bodies.get(&key) {
            return Some(result.clone());
        }
        let fn_type = self.ensure_overload_signature(module_path, index).ok()?;
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            unreachable!("only ever called with an index ensure_module_indexed confirmed is a function");
        };
        let mut analyzer = Analyzer::new(self, module_path.to_vec(), &[], (f.id, f.span));
        let checked = analyzer.check_function_body(f, &fn_type, f.id);
        let (errors, warnings) = analyzer.finish();
        if !errors.is_empty() {
            self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
        }
        let result = (CheckedItem::FunctionDefinition(checked?), warnings);
        self.overload_bodies.insert(key, result.clone());
        Some(result)
    }

    /// Compares every pair of `name`'s overload candidates (`indices`,
    /// already resolved into `signatures` at the same positions) by
    /// param-type list, ignoring parameter names -- an identical pair is a
    /// genuine duplicate (two calls could never be told apart), reported
    /// via the same `Redeclaration` diagnostic a same-shaped non-function
    /// collision already gets in `ensure_module_indexed`, not a new
    /// variant, since the underlying meaning ("this name already exists
    /// here") is identical.
    fn check_overload_duplicates(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        indices: &[usize],
        signatures: &[ResolvedFunctionType],
    ) {
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        for i in 1..indices.len() {
            for j in 0..i {
                let same_params =
                    signatures[i].params.iter().map(|(_, t)| t).eq(signatures[j].params.iter().map(|(_, t)| t));
                if same_params {
                    let (id, span) = item_id_span(&hir.items[indices[i]]);
                    let (_, previous) = item_id_span(&hir.items[indices[j]]);
                    self.module_errors.entry(module_path.to_vec()).or_default().push(AnalysisError::new(
                        id,
                        span,
                        AnalysisErrorKind::Redeclaration { name: name.clone(), previous: Some(previous) },
                    ));
                    break;
                }
            }
        }
    }

    /// Every reachable module's every error recorded so far, drained into
    /// the `Vec<CompileError>` shape `compile` returns on failure.
    fn drain_errors(&mut self, reachable: &[Vec<Ident>]) -> Vec<CompileError> {
        reachable
            .iter()
            .filter_map(|path| {
                let errors = self.module_errors.remove(path)?;
                (!errors.is_empty()).then(|| CompileError::Analysis { module: path.clone(), errors })
            })
            .collect()
    }

    /// Compiles every module reachable from `entry`: discovers the
    /// reachable set, resolves every one's every non-generic item's
    /// signature (phase 1 -- see `ensure_item`; same- and cross-module
    /// forward references and self-references all resolve regardless of
    /// declaration order or which module they cross, and a same- or
    /// cross-module by-value cycle is rejected right at the item that closes
    /// it, without affecting any other item), then checks every one's every
    /// non-generic item's body (phase 2, now that every reachable signature
    /// is guaranteed to already exist). A *generic* template is skipped by
    /// both sweeps (it has no concrete signature/body of its own to check --
    /// only a specific instantiation does, triggered lazily by whatever use
    /// site first needs it, during either phase); every instantiation
    /// discovered along the way is merged into its originating module's
    /// item list in the final assembly step below, once both phases have
    /// fully finished (so every instantiation, however late it was
    /// discovered, is guaranteed already present in `generic_instantiations`
    /// by then). Mirrors the identical split `omega_codegen::Codegen` does
    /// at the codegen layer, for the same underlying reason (a cross-module
    /// call in either direction must never need something that isn't ready
    /// yet).
    pub fn compile(&mut self, entry: &[Ident]) -> Result<CompiledProgram, Vec<CompileError>> {
        let resolve = |e: ResolveError| vec![CompileError::Resolve { error: e, importer: None }];
        let reachable = self.discover_reachable(entry).map_err(|e| vec![e])?;
        // Only ever swept eagerly for *local* modules -- an extern module's
        // items resolve lazily, on demand, exactly like a generic
        // instantiation already does (see `is_extern_module`'s doc comment):
        // "scanned, not compiled" means its signatures are fully available
        // to whatever local code actually references them, but nothing in
        // it is ever eagerly resolved, body-checked, or handed to codegen
        // for a *definition* -- that's the separate `omgc` invocation
        // compiling it standalone's job. Errors purely internal to an
        // extern module (e.g. one of its own broken imports that nothing
        // local ever actually needed) are correspondingly never surfaced by
        // this compilation either -- see `drain_errors`'s call sites below,
        // both scoped to `local_reachable`, not `reachable`.
        let local_reachable: Vec<Vec<Ident>> =
            reachable.iter().filter(|p| !self.is_extern_module(p)).cloned().collect();

        for path in &local_reachable {
            self.ensure_module_indexed(path).map_err(resolve)?;
            let overloaded_names: std::collections::HashSet<Ident> =
                self.function_overloads[path].keys().cloned().collect();
            let names: Vec<Ident> = self.local_items[path].keys().cloned().collect();
            for name in &names {
                // Handled below instead -- `ensure_item`'s `ItemKey` can
                // only ever address one item per name, so it would silently
                // only process the first-declared overload.
                if overloaded_names.contains(name) {
                    continue;
                }
                let generics = self.raw_item_generics(path, name).map_err(resolve)?;
                if !generics.is_empty() {
                    continue;
                }
                // Not itself a reference from inside any type -- nothing is
                // "in progress" yet at this point in the sweep, so
                // `indirect`'s distinction can't matter here; `true` just
                // means "no spurious cycle risk from the sweep itself."
                let _ = self.ensure_item(path, name, &[], true);
            }
            // Every overloaded name's every candidate signature -- resolved
            // eagerly here (unlike a generic instantiation, an overload set
            // is fully enumerable up front, so there's no need for
            // `check_generic_instantiation_body`'s on-demand trigger/
            // deferred-merge dance).
            for (name, indices) in &self.function_overloads[path].clone() {
                let signatures: Vec<ResolvedFunctionType> = indices
                    .iter()
                    .map(|&i| self.ensure_overload_signature(path, i))
                    .collect::<Result<_, _>>()
                    .map_err(resolve)?;
                self.check_overload_duplicates(path, name, indices, &signatures);
            }
        }

        let errors = self.drain_errors(&local_reachable);
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut modules = Vec::new();
        let mut warnings = Vec::new();
        for path in &reachable {
            let extern_module = self.is_extern_module(path);
            let mut items = Vec::new();

            // An extern module's *ordinary* (non-generic) items are never
            // body-checked or defined here -- only a generic instantiation
            // of one of its templates is (see the loop below this one):
            // nothing else will ever compile that exact instantiation, so
            // it must happen here, in whichever project actually asked for
            // it, even though the template itself lives in someone else's
            // project.
            if !extern_module {
                let hir = self.parsed.get(path).expect("reachable modules are always parsed").clone();
                let overloaded_names: std::collections::HashSet<Ident> =
                    self.function_overloads[path].keys().cloned().collect();
                for item in hir.items.iter().filter(|i| !matches!(i, HirItem::Import(_))) {
                    let Some(name) = item_name(item) else { continue };
                    // Handled below instead -- `check_item_body`'s `ItemKey`
                    // lookup would collide across every candidate sharing
                    // this name (see this loop's overload-sweep counterpart
                    // below).
                    if overloaded_names.contains(&name) {
                        continue;
                    }
                    let generics = self.raw_item_generics(path, &name).map_err(resolve)?;
                    if !generics.is_empty() {
                        continue;
                    }
                    if let Some((checked, item_warnings)) = self.check_item_body(path, &name, item, &[]) {
                        items.push(checked);
                        warnings.extend(item_warnings.into_iter().map(|w| (path.clone(), w)));
                    }
                }
                for indices in self.function_overloads[path].clone().into_values() {
                    for index in indices {
                        if let Some((checked, item_warnings)) = self.ensure_overload_body(path, index) {
                            items.push(checked);
                            warnings.extend(item_warnings.into_iter().map(|w| (path.clone(), w)));
                        }
                    }
                }
            }

            let module_id = *self.module_ids.get(path).expect("parsed modules always get an id");
            modules.push((path.clone(), CheckedModule { id: module_id, items }));
        }

        // Every generic instantiation discovered along the way (during
        // either phase above, from any module -- extern-owned templates
        // included, see this loop's own module list above) is merged in
        // here, only now that both phases have fully finished -- see
        // `compile`'s own doc comment for why this can't be folded into the
        // per-module loop above.
        for (path, checked_module) in modules.iter_mut() {
            for ((inst_path, _, _), (item, item_warnings)) in &self.generic_instantiations {
                if inst_path == path {
                    checked_module.items.push(item.clone());
                    warnings.extend(item_warnings.iter().map(|w| (path.clone(), w.clone())));
                }
            }
        }

        let errors = self.drain_errors(&local_reachable);
        if !errors.is_empty() {
            return Err(errors);
        }

        let extern_functions = self.collect_extern_functions();

        Ok(CompiledProgram { modules, entry: entry.to_vec(), warnings, extern_functions })
    }

    /// Every extern-owned, *non-generic* function/method actually
    /// referenced by this compilation -- everything codegen needs to
    /// declare (never define -- see `CompiledProgram::extern_functions`'s
    /// doc comment) a link against. Swept once, at the very end of
    /// `compile`, directly over whatever ended up in the already-populated
    /// per-item caches (`resolved_items` for free functions,
    /// `struct_cells`/`enum_cells`/`union_cells` for methods) -- nothing
    /// dedicated is tracked eagerly in `ensure_item`'s own hot path, since
    /// anything actually referenced is already sitting in these caches by
    /// construction. A *generic* instantiation of an extern template is
    /// deliberately excluded here (`type_args.is_empty()` guards): it's
    /// fully compiled locally instead (see `compile`'s own generic-
    /// instantiation merge step), since no other compilation will ever
    /// produce it.
    fn collect_extern_functions(&self) -> Vec<ExternFunctionRef> {
        let mut extern_functions = Vec::new();

        for ((module_path, name, type_args), (_, item)) in &self.resolved_items {
            if type_args.is_empty()
                && self.is_extern_module(module_path)
                && let ResolvedItem::Value { r#type: ResolvedType::Function(fn_type), storage: Storage::Function, decl_id } =
                    item
            {
                extern_functions.push(ExternFunctionRef {
                    decl_id: *decl_id,
                    module_path: module_path.clone(),
                    kind: ExternFunctionKind::Free(name.clone()),
                    fn_type: fn_type.clone(),
                });
            }
        }

        // Free-function *overloads* live in their own cache, addressed by
        // item index rather than `ItemKey` (see `overload_signatures`'s doc
        // comment) -- the function's own name/id are read back off the
        // parsed HIR at that same index.
        for ((module_path, index), fn_type) in &self.overload_signatures {
            if !self.is_extern_module(module_path) {
                continue;
            }
            let hir = self.parsed.get(module_path).expect("parsed by ensure_overload_signature");
            let HirItem::FunctionDefinition(f) = &hir.items[*index] else {
                unreachable!("overload_signatures only ever indexes a function");
            };
            extern_functions.push(ExternFunctionRef {
                decl_id: f.id,
                module_path: module_path.clone(),
                kind: ExternFunctionKind::Free(f.name.clone()),
                fn_type: fn_type.clone(),
            });
        }

        let method_cells = self
            .struct_cells
            .iter()
            .map(|(key, cell)| (key, cell.borrow().functions.clone()))
            .chain(self.enum_cells.iter().map(|(key, cell)| (key, cell.borrow().functions.clone())))
            .chain(self.union_cells.iter().map(|(key, cell)| (key, cell.borrow().functions.clone())));
        for ((module_path, type_name, type_args), functions) in method_cells {
            if !type_args.is_empty() || !self.is_extern_module(module_path) {
                continue;
            }
            for (method_name, method) in functions {
                extern_functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: module_path.clone(),
                    kind: ExternFunctionKind::Method { type_name: type_name.clone(), method_name },
                    fn_type: method.fn_type,
                });
            }
        }

        extern_functions
    }
}

impl ModuleResolver for Driver {
    fn resolve_import_alias(
        &mut self,
        module_path: &[Ident],
        alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        let Some((_, _, absolute)) = self.raw_imports.get(module_path).and_then(|m| m.get(alias)).cloned() else {
            return Ok(None);
        };
        self.resolve_import_alias_cached(module_path, alias, &absolute).map(Some)
    }

    fn import_alias_names(&mut self, module_path: &[Ident]) -> Vec<Ident> {
        if self.ensure_module_indexed(module_path).is_err() {
            return vec![];
        }
        self.raw_imports.get(module_path).map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    fn resolve_item(
        &mut self,
        absolute_path: &[Ident],
        type_args: &[ResolvedType],
        indirect: bool,
    ) -> Result<ResolvedItem, ResolveError> {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        self.ensure_item(module_path, item_name, type_args, indirect)
    }

    fn generic_function_signature(
        &mut self,
        absolute_path: &[Ident],
    ) -> Result<Option<GenericSignature>, ResolveError> {
        let Some((name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        // "Doesn't exist" is deferred to the ordinary call path, which
        // re-derives and reports it identically -- this query only ever
        // needs to say "not a generic function" either way.
        let Ok(index) = self.local_item_index(module_path, name) else {
            return Ok(None);
        };
        let hir = self.parsed.get(module_path).expect("parsed by local_item_index");
        let HirItem::FunctionDefinition(f) = &hir.items[index] else {
            return Ok(None);
        };
        if f.generics.is_empty() {
            return Ok(None);
        }
        Ok(Some(GenericSignature {
            generics: f.generics.clone(),
            params: f.params.iter().map(|p| p.r#type.clone()).collect(),
        }))
    }

    fn function_overload_signatures(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Option<Vec<(HirId, ResolvedFunctionType)>>, ResolveError> {
        // A module-resolution failure here doesn't mean this call is
        // broken -- it means `module_path` (the caller's naive "everything
        // but the last segment" split of an absolute path) isn't a real
        // module at all, which is exactly what a `Module::Type::function`
        // static-call path (its `module_path` would actually be
        // `[Module, Type]`) looks like from here. Swallowed the same way
        // `generic_function_signature` swallows it, for the identical
        // reason: "not a flat item path" just means "not this call's
        // concern," left for the ordinary path to resolve/report for real.
        if self.ensure_module_indexed(module_path).is_err() {
            return Ok(None);
        }
        let Some(indices) = self.function_overloads[module_path].get(name).cloned() else {
            return Ok(None);
        };
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        let mut candidates = Vec::with_capacity(indices.len());
        for index in indices {
            let HirItem::FunctionDefinition(f) = &hir.items[index] else {
                unreachable!("function_overloads only ever records function item indices");
            };
            candidates.push((f.id, self.ensure_overload_signature(module_path, index)?));
        }
        Ok(Some(candidates))
    }

    fn similar_item_name(
        &mut self,
        module_path: &[Ident],
        target: &Ident,
        namespace: ItemNamespace,
    ) -> Option<Ident> {
        // Purely advisory -- a module that can't even be indexed just
        // produces no suggestion (its own failure is reported elsewhere).
        if self.ensure_module_indexed(module_path).is_err() {
            return None;
        }
        let hir = self.parsed.get(module_path)?;
        let index = self.local_items.get(module_path)?;
        let candidates = index
            .iter()
            .filter(|&(_, &i)| match &hir.items[i] {
                HirItem::Struct(_) | HirItem::Enum(_) | HirItem::Union(_) => namespace == ItemNamespace::Type,
                HirItem::FunctionDefinition(_) | HirItem::Declaration(_) | HirItem::ExternDeclaration(_) => {
                    namespace == ItemNamespace::Value
                }
                HirItem::Import(_) => false,
            })
            .map(|(name, _)| name);
        best_match(target, candidates)
    }
}
