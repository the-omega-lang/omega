mod fs_resolve;

use fs_resolve::locate_module;
use omega_analyzer::analysis::Analyzer;
use omega_analyzer::checked::CheckedModule;
use omega_analyzer::error::AnalysisError;
use omega_analyzer::resolver::{ImportTarget, ModuleResolver, ModuleSignature, ResolveError, ResolvedItem};
use omega_hir::{HirItem, HirModule, ModuleId};
use omega_parser::prelude::{Ident, Path, SourceModule};
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
/// error), or ordinary semantic errors from one module's own body analysis.
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
    pub warnings: Vec<omega_analyzer::error::AnalysisWarning>,
}

/// A `signature_of` query's memoized state -- `InProgress` is the
/// white/gray/black cycle guard: a module whose signature collection is
/// already on the call stack is gray, and a second request for it before
/// the first finishes is a genuine cross-module cycle (see `Driver::
/// signature_of`'s doc comment), not a bug to recurse through.
enum SignatureState {
    InProgress,
    Done(Result<Rc<ModuleSignature>, ResolveError>),
}

/// Owns everything module-tree-shaped: filesystem discovery, a parsed-HIR
/// cache (each file parsed at most once, satisfying "avoid reanalyzing a
/// file just because another file imported it"), a signature cache with
/// module-granularity cycle detection, and the `ModuleResolver` glue
/// `omega-analyzer` calls back into. `omega-analyzer` itself never sees a
/// filesystem or a cache -- this is the only place that does.
pub struct Driver {
    roots: SearchRoots,
    next_module_id: u32,
    parsed: HashMap<Vec<Ident>, Rc<HirModule>>,
    module_ids: HashMap<Vec<Ident>, ModuleId>,
    signatures: HashMap<Vec<Ident>, SignatureState>,
}

impl Driver {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: SearchRoots(roots),
            next_module_id: 0,
            parsed: HashMap::new(),
            module_ids: HashMap::new(),
            signatures: HashMap::new(),
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

    /// A module's exported signature table, collected on first request and
    /// cached forever after -- the cross-module half of "avoid reanalyzing a
    /// file over and over," and the sole place cross-module cycles are
    /// caught: `path` is marked in-progress (gray) before recursing into
    /// whatever other modules its own signature collection needs, so a
    /// cycle (e.g. two structs in different modules referencing each other
    /// by value) is a *second* request for a still-gray module, reported as
    /// `ResolveError::Cycle` rather than infinite recursion. Deliberately
    /// module-granularity, not per-item: a foreign module's signature is
    /// only ever consumed as one atomic unit by `resolve_item`/
    /// `resolve_import`, so that's the coarsest -- and correct -- unit to
    /// guard.
    fn signature_of(&mut self, path: &[Ident]) -> Result<Rc<ModuleSignature>, ResolveError> {
        match self.signatures.get(path) {
            Some(SignatureState::Done(result)) => return result.clone(),
            Some(SignatureState::InProgress) => return Err(ResolveError::Cycle(vec![path.to_vec()])),
            None => {}
        }

        self.signatures.insert(path.to_vec(), SignatureState::InProgress);

        let result = self.parse_module(path).and_then(|hir| {
            let mut analyzer = Analyzer::new(&mut *self);
            analyzer.collect_signatures(&hir).map(Rc::new).map_err(|errors| {
                // Signature collection failing isn't itself a resolution
                // error shape (`ResolveError` has no "analysis failed"
                // variant, deliberately -- see `ModuleResolver`'s doc
                // comment: it only ever talks about module-tree shape).
                // Surfacing the *first* error is enough to explain why this
                // module's signature isn't available to an importer; the
                // full list is still reported once this module is itself
                // reachable and analyzed for its own bodies in `compile`.
                errors.into_iter().next().map(|e| ResolveError::LoadFailed {
                    path: path.to_vec(),
                    message: e.to_string(),
                }).expect("collect_signatures only returns Err with a non-empty error list")
            })
        });

        self.signatures.insert(path.to_vec(), SignatureState::Done(result.clone()));
        result
    }

    /// Compiles every module reachable from `entry`: discovers the
    /// reachable set, collects every one's signature (pass 1 -- see
    /// `signature_of`), then analyzes every one's bodies (pass 2, now that
    /// every reachable signature is guaranteed to already exist, regardless
    /// of which module imports which). Mirrors the identical split
    /// `omega_codegen::Codegen` does at the codegen layer, for the same
    /// underlying reason (a cross-module call in either direction must
    /// never need something that isn't ready yet).
    pub fn compile(&mut self, entry: &[Ident]) -> Result<CompiledProgram, Vec<CompileError>> {
        let reachable = self.discover_reachable(entry).map_err(|e| vec![CompileError::Resolve(e)])?;

        for path in &reachable {
            if let Err(e) = self.signature_of(path) {
                return Err(vec![CompileError::Resolve(e)]);
            }
        }

        let mut modules = Vec::new();
        let mut warnings = Vec::new();
        let mut errors = Vec::new();
        for path in &reachable {
            let hir = self.parsed.get(path).expect("reachable modules are always parsed by discover_reachable").clone();
            let analyzer = Analyzer::new(&mut *self);
            match analyzer.analyze_bodies(&hir) {
                Ok((checked, module_warnings)) => {
                    modules.push((path.clone(), checked));
                    warnings.extend(module_warnings);
                }
                Err(module_errors) => errors.push(CompileError::Analysis { module: path.clone(), errors: module_errors }),
            }
        }

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
        let signature = self.signature_of(module_path)?;
        let entry = signature
            .items
            .get(item_name)
            .ok_or_else(|| ResolveError::UnknownItem { module: module_path.to_vec(), item: item_name.clone() })?;

        match entry.visibility {
            omega_analyzer::resolver::Visibility::Public => Ok(ImportTarget::Item(entry.item.clone())),
        }
    }

    fn resolve_item(&mut self, absolute_path: &[Ident]) -> Result<ResolvedItem, ResolveError> {
        let Some((item_name, module_path)) = absolute_path.split_last() else {
            return Err(ResolveError::UnknownModule(absolute_path.to_vec()));
        };
        let signature = self.signature_of(module_path)?;
        let entry = signature
            .items
            .get(item_name)
            .ok_or_else(|| ResolveError::UnknownItem { module: module_path.to_vec(), item: item_name.clone() })?;

        match entry.visibility {
            omega_analyzer::resolver::Visibility::Public => Ok(entry.item.clone()),
        }
    }
}
