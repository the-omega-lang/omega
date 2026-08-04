//! The module tree: parsing a module's file at most once, and indexing its
//! top-level names and import aliases. What a compilation actually looks at
//! is decided elsewhere now -- the local package's own set comes straight
//! from the filesystem (`Driver::local_module_paths`), and `core`'s own
//! tree, wherever it's registered, does too (`ModuleRoots::core_modules`) --
//! this module no longer walks an import graph to find anything.

use crate::error::{CompileError, ImportSite};
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use indexmap::map::Entry;
use omega_analyzer::analysis::{item_id_span, item_name};
use omega_analyzer::annotations::{self, ItemKind};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolver::ResolveError;
use omega_diagnostics::{SourceFile, Span};
use omega_hir::{HirGenericParam, HirId, HirItem, HirModule, ModuleId};
use omega_parser::macros::MacroError;
use omega_parser::prelude::{Ident, ImportRoot, ParseError, Path, SourceModule};
use std::collections::HashMap;
use std::rc::Rc;

/// One module's parsed content, plus the lazily-built index over its own
/// top-level names.
pub(crate) struct ParsedModule {
    pub id: ModuleId,
    pub hir: Rc<HirModule>,
    /// Whether this module is *directory-shaped* (has children of its own --
    /// see `fs_resolve::ModuleLocation`). This is exactly what
    /// `Driver::relative_base` needs: a directory-shaped module's children
    /// live directly under it (its own path *is* its relative base), while a
    /// leaf file's siblings live in its parent directory.
    pub directory_shaped: bool,
    /// Built once, the first time anything looks a name up in this module.
    pub index: Option<ModuleIndex>,
}

/// A module's own top-level names, indexed. Purely structural: building this
/// resolves nothing and touches no other module.
pub(crate) struct ModuleIndex {
    /// Item name -> its position in `HirModule::items`. For an overloaded
    /// name this points at the first declaration only; the overload path uses
    /// `overloads` instead.
    pub items: IndexMap<Ident, usize>,
    /// Every name that declares *more than one* function, with all of their
    /// positions. A name absent here is never overloaded -- it's either not a
    /// function or an ordinary one-item name, both served by `items`.
    pub overloads: IndexMap<Ident, Vec<usize>>,
    /// Every `import` statement, keyed by the alias it binds.
    pub imports: IndexMap<Ident, ImportEntry>,
}

impl ModuleIndex {
    /// Every top-level item a single item query can address, with its
    /// position -- i.e. every one that isn't an overload group, which needs
    /// the per-candidate path instead (an `ItemKey` can only ever address one
    /// item per name, so it would silently only ever reach the first-declared
    /// candidate).
    ///
    /// In declaration order, which is what makes both whole-program sweeps
    /// deterministic build-to-build.
    pub fn plain_items(&self) -> Vec<(Ident, usize)> {
        self.items
            .iter()
            .filter(|(name, _)| !self.overloads.contains_key(*name))
            .map(|(name, &index)| (name.clone(), index))
            .collect()
    }
}

/// One `import` statement, reduced to what resolution later needs.
///
/// Computing `target` (the absolute path the statement names) needs no
/// signature lookup, no recursion, and no filesystem access -- only deciding
/// what that path *is* (a module vs. an item) is deferred, lazily, to
/// `Driver::resolve_alias`.
pub(crate) struct ImportEntry {
    pub id: HirId,
    pub span: Span,
    /// The absolute module path this import names.
    pub target: ModulePath,
    /// This import's own `@suppress(...)` list, resolved here because an
    /// import has no per-item analysis pass of its own for `UnusedImport` to
    /// hook into otherwise.
    pub suppress: Vec<Ident>,
    /// The `reveal` modifier, needed when the target is finally resolved.
    pub reveal: bool,
}

/// Why a module's own file never produced usable HIR. Stashed structurally
/// (rather than flattened into a `ResolveError` message) because
/// `parse_module`'s callers only speak `ResolveError`; `Driver::load_failure`
/// turns these back into first-class `CompileError`s.
pub(crate) enum LoadFailure {
    Parse(Vec<ParseError>),
    MacroExpansion(MacroError),
}

/// Every module this compilation has touched: its HIR (parsed at most once),
/// its source text (kept for diagnostic rendering long after parsing), and
/// why it failed to load if it did.
#[derive(Default)]
pub(crate) struct ModuleStore {
    modules: HashMap<ModulePath, ParsedModule>,
    /// Recorded the moment a file is read -- before parsing is even
    /// attempted, so a module that fails to parse can still render its own
    /// error snippets. Deliberately not part of `ParsedModule`, which only
    /// exists for modules that parsed successfully.
    sources: HashMap<ModulePath, Rc<SourceFile>>,
    failures: HashMap<ModulePath, LoadFailure>,
    next_id: u32,
}

impl ModuleStore {
    fn fresh_id(&mut self) -> ModuleId {
        let id = ModuleId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn get(&self, path: &[Ident]) -> Option<&ParsedModule> {
        self.modules.get(path)
    }

    /// A module known to be parsed already. Every caller reaches this through
    /// a path that parsed it first (an item index, a reachability walk), so a
    /// miss is a driver bug, not a user error.
    pub fn parsed(&self, path: &[Ident]) -> &ParsedModule {
        self.modules.get(path).expect("module was parsed before this point")
    }

    pub fn hir(&self, path: &[Ident]) -> Rc<HirModule> {
        self.parsed(path).hir.clone()
    }

    /// A module's index, which is present for every module anything has
    /// looked a name up in (see `Driver::ensure_module_indexed`).
    pub fn index(&self, path: &[Ident]) -> &ModuleIndex {
        self.parsed(path).index.as_ref().expect("module was indexed before this point")
    }

    pub fn set_index(&mut self, path: &[Ident], index: ModuleIndex) {
        self.modules.get_mut(path).expect("module was parsed before this point").index = Some(index);
    }

    pub fn set_imports(&mut self, path: &[Ident], imports: IndexMap<Ident, ImportEntry>) {
        self.modules.get_mut(path).expect("module was indexed before this point").index.as_mut()
            .expect("module was indexed before this point")
            .imports = imports;
    }

    pub fn is_indexed(&self, path: &[Ident]) -> bool {
        self.modules.get(path).is_some_and(|m| m.index.is_some())
    }

    /// One top-level item's raw HIR, by name -- `None` when the module isn't
    /// indexed or has no such name.
    pub fn item(&self, path: &[Ident], name: &Ident) -> Option<&HirItem> {
        let module = self.modules.get(path)?;
        let index = *module.index.as_ref()?.items.get(name)?;
        module.hir.items.get(index)
    }

    pub fn source(&self, path: &[Ident]) -> Option<Rc<SourceFile>> {
        self.sources.get(path).cloned()
    }

    pub fn take_failure(&mut self, path: &[Ident]) -> Option<LoadFailure> {
        self.failures.remove(path)
    }
}

impl Driver {
    /// Parses (and lowers) `path`'s own file, memoized -- the mechanism behind
    /// "only resolve things that are imported" (a module is parsed on demand,
    /// the first time something needs it) and "never reanalyze a file twice".
    /// A directory-shaped module with no own file (a pure namespace, e.g.
    /// `mymodule/` with no `mymodule/mymodule.omg`) is a valid, empty module,
    /// not an error.
    pub(crate) fn parse_module(&mut self, path: &[Ident]) -> Result<Rc<HirModule>, ResolveError> {
        if let Some(module) = self.modules.get(path) {
            return Ok(module.hir.clone());
        }

        let location = self.roots.locate(path)?;
        let id = self.modules.fresh_id();
        let hir = match location.own_file {
            None => HirModule { id, items: vec![] },
            Some(file) => {
                let source = std::fs::read_to_string(&file)
                    .map_err(|e| ResolveError::LoadFailed { path: path.to_vec(), message: e.to_string() })?;
                self.modules
                    .sources
                    .insert(path.to_vec(), Rc::new(SourceFile::new(file.display().to_string(), source.as_str())));
                // Parse/macro failures stash their real, structured errors and
                // return a `LoadFailed` whose message is only a fallback --
                // `load_failure` recognizes the stash and reports the
                // structured form instead.
                let ast = SourceModule::parse(&source).map_err(|errors| {
                    self.modules.failures.insert(path.to_vec(), LoadFailure::Parse(errors));
                    ResolveError::LoadFailed { path: path.to_vec(), message: "the module has syntax errors".into() }
                })?;
                let ast = omega_parser::macros::expand(ast).map_err(|e| {
                    self.modules.failures.insert(path.to_vec(), LoadFailure::MacroExpansion(e));
                    ResolveError::LoadFailed { path: path.to_vec(), message: "macro expansion failed".into() }
                })?;
                omega_hir::lower_module(id, &ast)
            }
        };

        let hir = Rc::new(hir);
        self.modules.modules.insert(
            path.to_vec(),
            ParsedModule {
                id,
                hir: hir.clone(),
                directory_shaped: location.children_dir.is_some(),
                index: None,
            },
        );
        Ok(hir)
    }

    /// Turns a module-load failure into its first-class `CompileError`: the
    /// stashed parse/macro-expansion errors when that's what actually went
    /// wrong, or the resolve error itself, tagged with the importing site.
    pub(crate) fn load_failure(
        &mut self,
        module: &[Ident],
        error: ResolveError,
        importer: Option<ImportSite>,
    ) -> CompileError {
        match self.modules.take_failure(module) {
            Some(LoadFailure::Parse(errors)) => CompileError::Parse { module: module.to_vec(), errors },
            Some(LoadFailure::MacroExpansion(error)) => {
                CompileError::MacroExpansion { module: module.to_vec(), error }
            }
            None => CompileError::Resolve { error, importer },
        }
    }

    /// The parsed source of `module`, for rendering its diagnostics --
    /// present for every module that got as far as being read off disk.
    pub fn source_file(&self, module: &[Ident]) -> Option<Rc<SourceFile>> {
        self.modules.source(module)
    }

    /// Builds (once) module `path`'s index of top-level names and import
    /// aliases, recording a `Redeclaration` error for each duplicate of
    /// either.
    pub(crate) fn ensure_module_indexed(&mut self, path: &[Ident]) -> Result<(), ResolveError> {
        if self.modules.is_indexed(path) {
            return Ok(());
        }
        let hir = self.parse_module(path)?;
        let (items, overloads) = self.index_items(path, &hir);
        // Published before the imports are indexed, deliberately: indexing an
        // import resolves its annotations, which runs an `Analyzer`, which
        // could in principle ask this very module for a name again. Marking
        // the module indexed here makes that re-entry return immediately
        // (finding no aliases, so the caller falls back to "not an alias")
        // instead of recursing forever.
        self.modules.set_index(path, ModuleIndex { items, overloads, imports: IndexMap::new() });
        let imports = self.index_imports(path, &hir);
        self.modules.set_imports(path, imports);
        Ok(())
    }

    /// The name -> position index over a module's own top-level items, and
    /// the subset of those names that are overload groups.
    fn index_items(
        &mut self,
        path: &[Ident],
        hir: &HirModule,
    ) -> (IndexMap<Ident, usize>, IndexMap<Ident, Vec<usize>>) {
        let mut items: IndexMap<Ident, usize> = IndexMap::new();
        let mut overloads: IndexMap<Ident, Vec<usize>> = IndexMap::new();
        let is_function = |i: usize| matches!(&hir.items[i], HirItem::FunctionDefinition(_));

        for (i, item) in hir.items.iter().enumerate() {
            let Some(name) = item_name(item) else { continue };
            let first_index = match items.entry(name.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(i);
                    continue;
                }
                Entry::Occupied(first) => *first.get(),
            };
            if is_function(i) && is_function(first_index) {
                // A valid overload *candidate*, not a redeclaration. Whether
                // it's genuinely distinct (a different signature) is checked
                // once every candidate's signature is resolved (see
                // `Driver::check_overload_duplicates`) -- nothing here has
                // access to param types yet.
                overloads.entry(name).or_insert_with(|| vec![first_index]).push(i);
            } else {
                let (id, span) = item_id_span(item);
                let (_, previous) = item_id_span(&hir.items[first_index]);
                self.diagnostics.error(
                    path,
                    AnalysisError::new(id, span, AnalysisErrorKind::Redeclaration { name, previous: Some(previous) }),
                );
            }
        }
        (items, overloads)
    }

    /// The alias -> import index over a module's own `import` statements.
    fn index_imports(&mut self, path: &[Ident], hir: &HirModule) -> IndexMap<Ident, ImportEntry> {
        let mut imports: IndexMap<Ident, ImportEntry> = IndexMap::new();
        for item in &hir.items {
            let HirItem::Import(import) = item else { continue };
            let alias = import.path.tail.last().cloned().unwrap_or_else(|| import.path.head.clone());
            let target = match self.import_absolute_path(path, import.root, &import.path) {
                Ok(target) => target,
                Err(e) => {
                    self.diagnostics.error(
                        path,
                        AnalysisError::new(import.id, import.span, AnalysisErrorKind::ModuleResolution(e)),
                    );
                    continue;
                }
            };
            // `@suppress` is the only annotation an import can carry
            // (`ItemKind::Import`); anything else records its own
            // `AnnotationNotApplicable`.
            let annotations = import.annotations.clone();
            let suppress = self
                .analyze(path, &[], (import.id, import.span), |analyzer| {
                    annotations::resolve(analyzer, import.id, &annotations, ItemKind::Import, false, false).suppress
                });

            match imports.entry(alias) {
                Entry::Occupied(existing) => {
                    let previous = existing.get().span;
                    let name = existing.key().clone();
                    self.diagnostics.error(
                        path,
                        AnalysisError::new(
                            import.id,
                            import.span,
                            AnalysisErrorKind::Redeclaration { name, previous: Some(previous) },
                        ),
                    );
                }
                Entry::Vacant(entry) => {
                    entry.insert(ImportEntry {
                        id: import.id,
                        span: import.span,
                        target,
                        suppress,
                        reveal: import.reveal,
                    });
                }
            }
        }
        imports
    }

    /// Module `path`'s item `name`'s position in its own `HirModule::items`
    /// -- indexes the module first if needed.
    pub(crate) fn local_item_index(&mut self, module_path: &[Ident], name: &Ident) -> Result<usize, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        self.modules
            .index(module_path)
            .items
            .get(name)
            .copied()
            .ok_or_else(|| ResolveError::UnknownItem { module: module_path.to_vec(), item: name.clone() })
    }

    /// An item's own declared generic parameters (empty = non-generic), with
    /// no analysis or instantiation triggered -- just a HIR field read behind
    /// the module index. The single source of truth for every "is this
    /// generic" check: an item import (which supplies no type arguments, so
    /// must not eagerly instantiate), `compile`'s sweeps (which must skip an
    /// uninstantiated template), and `ensure_item`'s arg-count validation.
    pub(crate) fn item_generics(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<Vec<HirGenericParam>, ResolveError> {
        let index = self.local_item_index(module_path, name)?;
        Ok(match &self.modules.parsed(module_path).hir.items[index] {
            HirItem::Struct(s) => s.generics.clone(),
            HirItem::Enum(e) => e.generics.clone(),
            HirItem::Union(u) => u.generics.clone(),
            HirItem::FunctionDefinition(f) => f.generics.clone(),
            HirItem::Spec(sp) => sp.generics.clone(),
            HirItem::Declaration(_)
            | HirItem::DeclarationWithInit(..)
            | HirItem::ExternDeclaration(_)
            | HirItem::Walrus(_) => vec![],
            HirItem::Import(_) => unreachable!("imports are never indexed into a module's items"),
        })
    }

    /// Whether an item is a *generic template* -- one that has no concrete
    /// signature or body of its own until some use site instantiates it.
    pub(crate) fn is_generic_template(&mut self, module_path: &[Ident], name: &Ident) -> Result<bool, ResolveError> {
        Ok(!self.item_generics(module_path, name)?.is_empty())
    }

    /// Where `module_path`'s own *unrooted* (`ImportRoot::Local`) imports
    /// start looking -- see `ParsedModule::directory_shaped`. Always called
    /// for an already-parsed module (an import can only be resolved for a
    /// module whose own statements are in hand).
    fn relative_base(&self, module_path: &[Ident]) -> ModulePath {
        if self.modules.parsed(module_path).directory_shaped {
            module_path.to_vec()
        } else {
            module_path[..module_path.len().saturating_sub(1)].to_vec()
        }
    }

    /// The absolute module path one `import` statement names, given the
    /// *importing* module's own path -- pure path arithmetic, no recursive
    /// item resolution and no filesystem access. See `ImportRoot` for what
    /// each variant means.
    fn import_absolute_path(
        &self,
        importer: &[Ident],
        root: ImportRoot,
        path: &Path,
    ) -> Result<ModulePath, ResolveError> {
        match root {
            ImportRoot::Local => {
                let mut absolute = self.relative_base(importer);
                absolute.extend(path.segments());
                Ok(absolute)
            }
            // "Root of *my* current project" -- if the importer is itself part
            // of an extern project (its path leads with that project's own
            // declared module name), re-prepend that name so the result stays
            // anchored to *that* project's root rather than silently falling
            // back to the local one.
            ImportRoot::ProjectRoot => {
                let mut absolute = Vec::new();
                if self.roots.is_extern(importer) {
                    absolute.push(importer[0].clone());
                }
                absolute.extend(path.segments());
                Ok(absolute)
            }
            // `path.head` is the extern module's own declared name (the same
            // `Ident` `--extern` registered it under -- no separate local
            // alias to translate). Checked eagerly here, rather than left to
            // an ordinary not-found, so a typo'd or forgotten `--extern` flag
            // gets its own precise diagnostic.
            ImportRoot::Extern => {
                if !self.roots.has_extern(&path.head) {
                    return Err(ResolveError::UnknownExtern(path.head.clone()));
                }
                Ok(std::iter::once(path.head.clone()).chain(path.tail.iter().cloned()).collect())
            }
        }
    }
}
