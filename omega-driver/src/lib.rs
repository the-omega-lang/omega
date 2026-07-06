mod fs_resolve;

use fs_resolve::locate_module;
use omega_analyzer::analysis::{item_id_span, item_name, Analyzer};
use omega_analyzer::checked::{CheckedItem, CheckedModule, Storage};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind, AnalysisWarning};
use omega_analyzer::resolved_type::{ResolvedStructType, ResolvedType};
use omega_analyzer::resolver::{ImportTarget, ModuleResolver, ResolveError, ResolvedImport, ResolvedItem, Visibility};
use omega_hir::{HirItem, HirModule, ModuleId};
use omega_parser::prelude::{Ident, Path, SourceModule};
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

/// Where to look for a module path on disk, tried in order, first match
/// wins -- deliberately dumb (no per-root package identity/namespacing) so
/// a real package system later just means adding entries and namespacing
/// logic behind this one type, not touching any call site. Exactly one
/// entry today (the entry file's parent directory); see `Driver::new`.
#[derive(Debug, Clone)]
pub struct SearchRoots(pub Vec<PathBuf>);

/// Everything that can go wrong compiling a multi-module program: a
/// module-resolution failure (unknown/ambiguous/cyclic module, a load
/// error), or ordinary semantic errors from one module's own signature/body
/// analysis.
#[derive(Debug)]
pub enum CompileError {
    Resolve(ResolveError),
    Analysis { module: Vec<Ident>, errors: Vec<AnalysisError> },
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{e}"),
            Self::Analysis { module, errors } => {
                let path = module.iter().map(|i| i.as_ref()).collect::<Vec<_>>().join("::");
                for error in errors {
                    writeln!(f, "{path}: {error}")?;
                }
                Ok(())
            }
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
    pub warnings: Vec<AnalysisWarning>,
}

/// A `(module_path, name)` query's memoized state -- `InProgress` is the
/// white/gray/black cycle guard: an item whose signature collection is
/// already on the call stack is gray, and a second request for it before the
/// first finishes is either fine (an indirect, pointer reference) or a
/// genuine cycle (a direct, by-value one) -- see `Driver::ensure_item`.
/// Deliberately item-granular, not module-granular: a foreign module's
/// signature used to be consumed as one atomic unit, but there's no longer
/// any such unit -- every top-level item (same-module or cross-module,
/// no difference now) is its own independent query, so one bad item never
/// poisons an unrelated sibling's.
enum QueryState {
    InProgress,
    Done,
}

/// A module's resolved-import-list query's memoized state -- its own guard,
/// separate from `QueryState`, because resolving one module's *item-style*
/// imports (`import b::thing;`, unlike a whole-module import, which is a
/// pure filesystem check needing no signature at all) can itself need
/// another module's imports resolved first. If module A imports an item
/// from B and B imports an item from A, building A's list triggers building
/// B's, which triggers building A's again -- *before* either module has any
/// per-item `QueryState` entry to catch it. This guard is what turns that
/// into a clean `ResolveError::Cycle` instead of unbounded recursion.
enum ImportCacheState {
    InProgress,
    Done(Result<Rc<Vec<ResolvedImport>>, ResolveError>),
}

/// Owns everything module-tree-shaped: filesystem discovery, a parsed-HIR
/// cache (each file parsed at most once), and the global, item-granular
/// query that replaces the old per-module signature cache -- see
/// `ensure_item`, the one mechanism now behind both same-module and
/// cross-module resolution (`omega_analyzer::Context::resolve_type`'s
/// unqualified-miss fallback, and a qualified reference, both end up here).
pub struct Driver {
    roots: SearchRoots,
    next_module_id: u32,
    parsed: HashMap<Vec<Ident>, Rc<HirModule>>,
    module_ids: HashMap<Vec<Ident>, ModuleId>,
    /// Every module's top-level items, indexed by name -- built once, the
    /// first time a module is touched (alongside duplicate-name detection,
    /// folded into `module_errors`); this is what `ensure_item` looks a name
    /// up in to find *what* to analyze the first time it's asked for.
    local_items: HashMap<Vec<Ident>, HashMap<Ident, usize>>,
    imports: HashMap<Vec<Ident>, ImportCacheState>,
    /// Every struct's shared identity cell, decoupled from any one module's
    /// analysis -- created the moment *anyone* (same-module or foreign)
    /// first asks about a given struct, independent of whether its own
    /// module has started resolving it yet. This is what lets an indirect
    /// (pointer) reference to a struct that's mid-collection -- anywhere,
    /// same module or a different one -- be served immediately, without
    /// needing exclusive access to whatever is currently building it.
    struct_cells: HashMap<(Vec<Ident>, Ident), Rc<RefCell<ResolvedStructType>>>,
    query_state: HashMap<(Vec<Ident>, Ident), QueryState>,
    /// Every item that finished its query successfully -- absent for one
    /// that's `Done` but failed (see `ensure_item`); the real diagnostics
    /// for those live in `module_errors` instead.
    resolved_items: HashMap<(Vec<Ident>, Ident), (Visibility, ResolvedItem)>,
    /// Every `AnalysisError` produced so far, keyed by the module it belongs
    /// to -- accumulated across both the signature phase (`ensure_item`) and
    /// the body phase (`compile`'s second pass), since neither one is a
    /// single long-lived whole-module `Analyzer` pass anymore.
    module_errors: HashMap<Vec<Ident>, Vec<AnalysisError>>,
}

impl Driver {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: SearchRoots(roots),
            next_module_id: 0,
            parsed: HashMap::new(),
            module_ids: HashMap::new(),
            local_items: HashMap::new(),
            imports: HashMap::new(),
            struct_cells: HashMap::new(),
            query_state: HashMap::new(),
            resolved_items: HashMap::new(),
            module_errors: HashMap::new(),
        }
    }

    fn fresh_module_id(&mut self) -> ModuleId {
        let id = ModuleId(self.next_module_id);
        self.next_module_id += 1;
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

        let location = locate_module(&self.roots.0, path)?;
        let module_id = self.fresh_module_id();

        let hir = match location.own_file {
            None => HirModule { id: module_id, items: vec![] },
            Some(file) => {
                let source = std::fs::read_to_string(&file).map_err(|e| ResolveError::LoadFailed {
                    path: path.to_vec(),
                    message: e.to_string(),
                })?;
                let ast = SourceModule::parse(&source).map_err(|errors| ResolveError::LoadFailed {
                    path: path.to_vec(),
                    message: errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "),
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
    /// statement's raw path -- itself, if it names a real module (a
    /// whole-module import), otherwise its parent (an item import: only the
    /// item's *owning* module needs to be parsed, never a same-named
    /// own-file it doesn't need). Same undecidable-from-syntax-alone
    /// disambiguation `resolve_import` does at analysis time, but cheaper --
    /// a filesystem check is all "which file(s) must be parsed" needs; it
    /// doesn't require a signature lookup the way "what does this name
    /// resolve to" does.
    fn reachable_target(&self, import_path: &Path) -> Result<Vec<Ident>, ResolveError> {
        let segments = import_path.segments();
        match locate_module(&self.roots.0, &segments) {
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
            if locate_module(&self.roots.0, parent).is_ok() {
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
    fn discover_reachable(&mut self, entry: &[Ident]) -> Result<Vec<Vec<Ident>>, ResolveError> {
        let mut reachable = vec![entry.to_vec()];
        let mut worklist = vec![entry.to_vec()];
        let mut seen: std::collections::HashSet<Vec<Ident>> = std::collections::HashSet::from([entry.to_vec()]);

        while let Some(path) = worklist.pop() {
            let hir = self.parse_module(&path)?;
            for item in &hir.items {
                let HirItem::Import(import) = item else { continue };
                let target = self.reachable_target(&import.path)?;
                if seen.insert(target.clone()) {
                    reachable.push(target.clone());
                    worklist.push(target);
                }
            }
        }

        Ok(reachable)
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
        for (i, item) in hir.items.iter().enumerate() {
            let Some(name) = item_name(item) else { continue };
            match index.entry(name.clone()) {
                Entry::Occupied(_) => {
                    let (id, span) = item_id_span(item);
                    self.module_errors
                        .entry(path.to_vec())
                        .or_default()
                        .push(AnalysisError::new(id, span, AnalysisErrorKind::Redeclaration(name)));
                }
                Entry::Vacant(entry) => {
                    entry.insert(i);
                }
            }
        }
        self.local_items.insert(path.to_vec(), index);
        Ok(())
    }

    /// Module `path`'s resolved import list, built once and cached --
    /// cycle-guarded by `ImportCacheState` (see its doc comment). An
    /// individual import statement that fails to resolve for an ordinary
    /// reason (unknown module/item, ...) is recorded into `module_errors`
    /// and simply left out of the list, rather than failing the whole
    /// module's import cache -- the rest of the module can still be checked
    /// against whatever *did* resolve.
    fn resolved_imports(&mut self, path: &[Ident]) -> Result<Rc<Vec<ResolvedImport>>, ResolveError> {
        match self.imports.get(path) {
            Some(ImportCacheState::Done(result)) => return result.clone(),
            Some(ImportCacheState::InProgress) => return Err(ResolveError::Cycle(vec![path.to_vec()])),
            None => {}
        }

        self.imports.insert(path.to_vec(), ImportCacheState::InProgress);
        let result = self.compute_resolved_imports(path);
        self.imports.insert(path.to_vec(), ImportCacheState::Done(result.clone()));
        result
    }

    fn compute_resolved_imports(&mut self, path: &[Ident]) -> Result<Rc<Vec<ResolvedImport>>, ResolveError> {
        let hir = self.parse_module(path)?;
        let mut resolved = Vec::new();
        for item in &hir.items {
            let HirItem::Import(import) = item else { continue };
            let alias = import.path.tail.last().cloned().unwrap_or_else(|| import.path.head.clone());
            match self.resolve_import(&import.path) {
                Ok(target) => resolved.push(ResolvedImport { id: import.id, span: import.span, alias, target }),
                Err(e) => {
                    self.module_errors.entry(path.to_vec()).or_default().push(AnalysisError::new(
                        import.id,
                        import.span,
                        AnalysisErrorKind::ModuleResolution(e),
                    ));
                }
            }
        }
        Ok(Rc::new(resolved))
    }

    /// Gets (or creates) `name`'s shared identity cell in module `path` --
    /// see `struct_cells`'s doc comment. Always called with a real `id`
    /// (the struct's own `HirId`) the first time, from `compute_item`, right
    /// before this same struct is marked `InProgress` and analyzed, so
    /// nothing can observe a missing cell for a struct that's actually
    /// `InProgress` (see `ensure_item`'s indirect+in-progress branch).
    fn struct_cell(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        id: omega_hir::HirId,
    ) -> Rc<RefCell<ResolvedStructType>> {
        self.struct_cells
            .entry((module_path.to_vec(), name.clone()))
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedStructType {
                    id,
                    name: name.clone(),
                    fields: vec![],
                    functions: vec![],
                }))
            })
            .clone()
    }

    /// The one global query behind both same-module and cross-module
    /// resolution -- see `ModuleResolver::resolve_item`'s doc comment. A
    /// name already `Done` is a cache hit (successful or not); one found
    /// `InProgress` is either a legitimate indirect (pointer) reference to
    /// something still being built (served straight from `struct_cells`) or
    /// a genuine by-value cycle (`RecursiveTypeWithoutIndirection`); anything
    /// else is analyzed right here, on the spot, before this returns.
    pub fn ensure_item(&mut self, module_path: &[Ident], name: &Ident, indirect: bool) -> Result<ResolvedItem, ResolveError> {
        let key = (module_path.to_vec(), name.clone());

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
                    return Err(ResolveError::Cycle(vec![module_path.to_vec()]));
                }
                return Err(ResolveError::RecursiveTypeWithoutIndirection {
                    module: module_path.to_vec(),
                    item: name.clone(),
                });
            }
            None => {}
        }

        self.ensure_module_indexed(module_path)?;
        let Some(&index) = self.local_items.get(module_path).and_then(|idx| idx.get(name)) else {
            return Err(ResolveError::UnknownItem { module: module_path.to_vec(), item: name.clone() });
        };

        self.query_state.insert(key.clone(), QueryState::InProgress);
        let result = self.compute_item(module_path, name, index);
        self.query_state.insert(key.clone(), QueryState::Done);
        if let Ok(item) = &result {
            self.resolved_items.insert(key, (Visibility::Public, item.clone()));
        }
        result
    }

    /// Does the actual work `ensure_item` defers to the first time a name is
    /// requested: builds one throwaway `Analyzer` for this one item (seeded
    /// with the module's already-resolved imports), dispatches by item kind,
    /// and folds whatever errors it produced into `module_errors`. A struct's
    /// cell is fetched/created *before* the `Analyzer` runs, so a self- or
    /// mutually-referencing pointer field hit during field resolution finds
    /// it already there (`ensure_item`'s `InProgress` branch serves it).
    fn compute_item(&mut self, module_path: &[Ident], name: &Ident, index: usize) -> Result<ResolvedItem, ResolveError> {
        let imports = self.resolved_imports(module_path)?;
        let hir = self.parsed.get(module_path).expect("parsed by ensure_module_indexed").clone();
        let item = &hir.items[index];

        let (result, errors) = match item {
            HirItem::Declaration(decl) => {
                let mut analyzer = Analyzer::new(self, module_path.to_vec(), &imports);
                let checked = analyzer.analyze_declaration(decl, Storage::Global);
                let (errors, _warnings) = analyzer.finish();
                let result = checked
                    .map(|c| ResolvedItem::Value { r#type: c.r#type, storage: Storage::Global, decl_id: c.id });
                (result, errors)
            }
            HirItem::ExternDeclaration(decl) => {
                let mut analyzer = Analyzer::new(self, module_path.to_vec(), &imports);
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
                let mut analyzer = Analyzer::new(self, module_path.to_vec(), &imports);
                let checked = analyzer.collect_function_signature(f);
                let (errors, _warnings) = analyzer.finish();
                let result = checked.map(|fn_type| ResolvedItem::Value {
                    r#type: ResolvedType::Function(fn_type),
                    storage: Storage::Function,
                    decl_id: f.id,
                });
                (result, errors)
            }
            HirItem::Struct(s) => {
                let cell = self.struct_cell(module_path, name, s.id);
                let mut analyzer = Analyzer::new(self, module_path.to_vec(), &imports);
                let ok = analyzer.signature_of_struct(s, &cell);
                let (errors, _warnings) = analyzer.finish();
                let result = ok.map(|()| ResolvedItem::Type(ResolvedType::Struct(cell)));
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
    /// with the identifying fields already sitting on the `HirItem`.
    fn check_item_body(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
        item: &HirItem,
        imports: &[ResolvedImport],
    ) -> Option<(CheckedItem, Vec<AnalysisWarning>)> {
        let key = (module_path.to_vec(), name.clone());
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
                let checked = CheckedItem::ExternDeclaration(omega_analyzer::checked::CheckedExternDecl {
                    id: decl.id,
                    span: decl.span,
                    ident: decl.ident.clone(),
                    r#type,
                });
                Some((checked, vec![]))
            }
            HirItem::FunctionDefinition(f) => {
                let (_, resolved) = self.resolved_items.get(&key).expect("resolved in phase 1").clone();
                let ResolvedItem::Value { r#type: ResolvedType::Function(fn_type), .. } = resolved else {
                    unreachable!("a function's own resolved item is always ResolvedType::Function");
                };
                let mut analyzer = Analyzer::new(self, module_path.to_vec(), imports);
                let checked = analyzer.check_function_body(f, &fn_type);
                let (errors, warnings) = analyzer.finish();
                if !errors.is_empty() {
                    self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
                }
                checked.map(|c| (CheckedItem::FunctionDefinition(c), warnings))
            }
            HirItem::Struct(s) => {
                let cell = self.struct_cells.get(&key).expect("resolved in phase 1").clone();
                let mut analyzer = Analyzer::new(self, module_path.to_vec(), imports);
                let checked = analyzer.check_struct_body(s, &cell);
                let (errors, warnings) = analyzer.finish();
                if !errors.is_empty() {
                    self.module_errors.entry(module_path.to_vec()).or_default().extend(errors);
                }
                checked.map(|c| (CheckedItem::Struct(c), warnings))
            }
            HirItem::Import(_) => unreachable!("imports are filtered out before this is called"),
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
    /// reachable set, resolves every one's every item's signature (phase 1
    /// -- see `ensure_item`; same- and cross-module forward references and
    /// self-references all resolve regardless of declaration order or which
    /// module they cross, and a same- or cross-module by-value cycle is
    /// rejected right at the item that closes it, without affecting any
    /// other item), then checks every one's every item's body (phase 2, now
    /// that every reachable signature is guaranteed to already exist). Mirrors
    /// the identical split `omega_codegen::Codegen` does at the codegen
    /// layer, for the same underlying reason (a cross-module call in either
    /// direction must never need something that isn't ready yet).
    pub fn compile(&mut self, entry: &[Ident]) -> Result<CompiledProgram, Vec<CompileError>> {
        let reachable = self.discover_reachable(entry).map_err(|e| vec![CompileError::Resolve(e)])?;

        for path in &reachable {
            self.ensure_module_indexed(path).map_err(|e| vec![CompileError::Resolve(e)])?;
            let names: Vec<Ident> = self.local_items[path].keys().cloned().collect();
            for name in &names {
                // Not itself a reference from inside any type -- nothing is
                // "in progress" yet at this point in the sweep, so
                // `indirect`'s distinction can't matter here; `true` just
                // means "no spurious cycle risk from the sweep itself."
                let _ = self.ensure_item(path, name, true);
            }
        }

        let errors = self.drain_errors(&reachable);
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut modules = Vec::new();
        let mut warnings = Vec::new();
        for path in &reachable {
            let hir = self.parsed.get(path).expect("reachable modules are always parsed").clone();
            let imports = self.resolved_imports(path).map_err(|e| vec![CompileError::Resolve(e)])?;

            let mut items = Vec::new();
            for item in hir.items.iter().filter(|i| !matches!(i, HirItem::Import(_))) {
                let Some(name) = item_name(item) else { continue };
                if let Some((checked, item_warnings)) = self.check_item_body(path, &name, item, &imports) {
                    items.push(checked);
                    warnings.extend(item_warnings);
                }
            }

            let module_id = *self.module_ids.get(path).expect("parsed modules always get an id");
            modules.push((path.clone(), CheckedModule { id: module_id, items }));
        }

        let errors = self.drain_errors(&reachable);
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(CompiledProgram { modules, entry: entry.to_vec(), warnings })
    }
}

impl ModuleResolver for Driver {
    fn resolve_import(&mut self, path: &Path) -> Result<ImportTarget, ResolveError> {
        let segments = path.segments();

        match locate_module(&self.roots.0, &segments) {
            Ok(_) => return Ok(ImportTarget::Module(segments)),
            // Real regardless of whether this turns out to be a
            // whole-module or item import -- must surface here, not be
            // masked by the item-import fallback below (see
            // `Driver::reachable_target`'s identical fix).
            Err(e @ ResolveError::AmbiguousModule(_)) => return Err(e),
            Err(_) => {}
        }

        let Some((item_name, module_path)) = segments.split_last() else {
            return Err(ResolveError::UnknownModule(segments));
        };
        // Capturing "what does this alias refer to" never embeds anything
        // inline the way a struct field does -- always indirect.
        Ok(ImportTarget::Item(self.ensure_item(module_path, item_name, true)?))
    }

    fn resolve_item(&mut self, absolute_path: &[Ident], indirect: bool) -> Result<ResolvedItem, ResolveError> {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        self.ensure_item(module_path, item_name, indirect)
    }
}
