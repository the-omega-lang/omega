use crate::error::{CompileError, ImportSite};
use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use indexmap::map::Entry;
use omega_analyzer::analysis::{AnalysisSite, item_id_span, item_name};
use omega_analyzer::annotations::{self, ItemKind};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolver::ResolveError;
use omega_diagnostics::{SourceFile, Span};
use omega_hir::{HirGenericParam, HirId, HirItem, HirModule, ModuleId};
use omega_parser::macros::MacroError;
use omega_parser::prelude::{Ident, ImportRoot, Item, ParseError, Path, SourceModule};
use omega_parser::prelude::{MacroDefinitionStmt, Visibility};
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) struct ParsedModule {
    pub id: ModuleId,
    pub hir: Rc<HirModule>,
    pub index: Option<ModuleIndex>,
}

pub(crate) struct ModuleIndex {
    pub items: IndexMap<Ident, usize>,
    pub overloads: IndexMap<Ident, Vec<usize>>,
    pub imports: IndexMap<Ident, ImportEntry>,
}

impl ModuleIndex {
    pub fn plain_items(&self) -> Vec<(Ident, usize)> {
        self.items
            .iter()
            .filter(|(name, _)| !self.overloads.contains_key(*name))
            .map(|(name, &index)| (name.clone(), index))
            .collect()
    }
}

pub(crate) struct ImportEntry {
    pub id: HirId,
    pub span: Span,
    pub target: ModulePath,
    pub suppress: Vec<Ident>,
    pub reveal: bool,
}

pub(crate) enum LoadFailure {
    Parse(Vec<ParseError>),
    MacroExpansion(MacroError),
    Compile(CompileError),
}

#[derive(Default)]
pub(crate) struct ModuleStore {
    modules: HashMap<ModulePath, ParsedModule>,
    asts: HashMap<ModulePath, Rc<SourceModule>>,
    macro_defs: HashMap<ModulePath, Rc<HashMap<Ident, MacroDefinitionStmt>>>,
    macro_expansions: omega_parser::macros::ExpansionState,
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

    pub fn parsed(&self, path: &[Ident]) -> &ParsedModule {
        self.modules
            .get(path)
            .expect("module was parsed before this point")
    }

    pub fn hir(&self, path: &[Ident]) -> Rc<HirModule> {
        self.parsed(path).hir.clone()
    }

    pub fn macro_origin_module(&self, origin: omega_parser::prelude::Origin) -> Option<ModulePath> {
        self.macro_expansions
            .defining_module(origin)
            .map(ToOwned::to_owned)
    }

    pub fn macro_origin_visibility(
        &self,
        origin: omega_parser::prelude::Origin,
    ) -> Option<omega_parser::prelude::Visibility> {
        self.macro_expansions.macro_visibility(origin)
    }

    pub fn index(&self, path: &[Ident]) -> &ModuleIndex {
        self.parsed(path)
            .index
            .as_ref()
            .expect("module was indexed before this point")
    }

    pub fn set_index(&mut self, path: &[Ident], index: ModuleIndex) {
        self.modules
            .get_mut(path)
            .expect("module was parsed before this point")
            .index = Some(index);
    }

    pub fn set_imports(&mut self, path: &[Ident], imports: IndexMap<Ident, ImportEntry>) {
        self.modules
            .get_mut(path)
            .expect("module was indexed before this point")
            .index
            .as_mut()
            .expect("module was indexed before this point")
            .imports = imports;
    }

    pub fn is_indexed(&self, path: &[Ident]) -> bool {
        self.modules.get(path).is_some_and(|m| m.index.is_some())
    }

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
    fn ensure_ast(
        &mut self,
        path: &[Ident],
        file: &std::path::Path,
    ) -> Result<Rc<SourceModule>, ResolveError> {
        if let Some(ast) = self.modules.asts.get(path) {
            return Ok(ast.clone());
        }

        let source = std::fs::read_to_string(file).map_err(|e| ResolveError::LoadFailed {
            path: path.to_vec(),
            message: e.to_string(),
        })?;
        self.modules.sources.insert(
            path.to_vec(),
            Rc::new(SourceFile::new(file.display().to_string(), source.as_str())),
        );
        let ast = SourceModule::parse(&source).map_err(|errors| {
            self.modules
                .failures
                .insert(path.to_vec(), LoadFailure::Parse(errors));
            ResolveError::LoadFailed {
                path: path.to_vec(),
                message: "the module has syntax errors".into(),
            }
        })?;
        let ast = Rc::new(ast);
        self.modules.asts.insert(path.to_vec(), ast.clone());
        Ok(ast)
    }

    fn module_macros(
        &mut self,
        path: &[Ident],
    ) -> Result<Rc<HashMap<Ident, MacroDefinitionStmt>>, ResolveError> {
        if let Some(definitions) = self.modules.macro_defs.get(path) {
            return Ok(definitions.clone());
        }

        let location = self.roots.locate(path)?;
        let definitions = match location.own_file {
            None => HashMap::new(),
            Some(file) => {
                let ast = self.ensure_ast(path, &file)?;
                let mut definitions = HashMap::new();
                for node in &ast.nodes {
                    if let Item::MacroDefinition(definition) = &node.item {
                        let mut definition = definition.clone();
                        definition.defining_module = path.to_vec();
                        definitions.insert(definition.name.clone(), definition);
                    }
                }
                definitions
            }
        };
        let definitions = Rc::new(definitions);
        self.modules
            .macro_expansions
            .register_environment(path, definitions.as_ref());
        self.modules
            .macro_defs
            .insert(path.to_vec(), definitions.clone());
        Ok(definitions)
    }

    fn prelude_macros(&mut self) -> Result<Rc<HashMap<Ident, MacroDefinitionStmt>>, CompileError> {
        if let Some(definitions) = &self.prelude_macros {
            return Ok(definitions.clone());
        }

        let mut definitions = HashMap::new();
        let mut origins: HashMap<Ident, ModulePath> = HashMap::new();
        for module in self.roots.core_modules() {
            let module_definitions = match self.module_macros(&module) {
                Ok(definitions) => definitions,
                Err(error) => return Err(self.load_failure(&module, error, None)),
            };
            for definition in module_definitions.values() {
                if definition.visibility != Visibility::Exposed {
                    continue;
                }
                if let Some(first) = origins.get(&definition.name) {
                    return Err(CompileError::AmbiguousPreludeMacro {
                        name: definition.name.clone(),
                        first: first.clone(),
                        second: module,
                    });
                }
                origins.insert(definition.name.clone(), module.clone());
                definitions.insert(definition.name.clone(), definition.clone());
            }
        }

        let definitions = Rc::new(definitions);
        self.prelude_macros = Some(definitions.clone());
        Ok(definitions)
    }

    fn macro_env(
        &mut self,
        path: &[Ident],
    ) -> Result<HashMap<Ident, MacroDefinitionStmt>, CompileError> {
        let mut environment = (*self.prelude_macros()?).clone();
        if path.first().map(Ident::as_ref) == Some("core") {
            let own = self
                .module_macros(path)
                .map_err(|error| CompileError::Resolve {
                    error,
                    importer: None,
                })?;
            for name in own.keys() {
                environment.remove(name);
            }
        }

        let location = self
            .roots
            .locate(path)
            .map_err(|error| CompileError::Resolve {
                error,
                importer: None,
            })?;
        let imports = match location.own_file {
            None => vec![],
            Some(file) => {
                let ast = self
                    .ensure_ast(path, &file)
                    .map_err(|error| CompileError::Resolve {
                        error,
                        importer: None,
                    })?;
                ast.nodes
                    .iter()
                    .filter_map(|node| match &node.item {
                        Item::Import(import) => Some(import.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            }
        };

        for import in imports {
            let absolute = match self.import_absolute_path(path, import.root, &import.path) {
                Ok(path) => path,
                Err(_) => continue,
            };
            if self.roots.module_exists(&absolute) || absolute.len() < 2 {
                continue;
            }
            let module = absolute[..absolute.len() - 1].to_vec();
            let name = absolute.last().expect("non-empty import path").clone();
            if !self.roots.module_exists(&module) {
                continue;
            }
            let definitions = match self.module_macros(&module) {
                Ok(definitions) => definitions,
                Err(_) => continue,
            };
            let Some(definition) = definitions.get(&name) else {
                continue;
            };
            let visible = definition.visibility == Visibility::Exposed
                || (definition.visibility == Visibility::Shared
                    && path.first() == module.first());
            if visible {
                environment
                    .entry(name)
                    .or_insert_with(|| definition.clone());
            }
        }
        Ok(environment)
    }

    pub(crate) fn parse_module(&mut self, path: &[Ident]) -> Result<Rc<HirModule>, ResolveError> {
        if let Some(module) = self.modules.get(path) {
            return Ok(module.hir.clone());
        }

        let location = self.roots.locate(path)?;
        let id = self.modules.fresh_id();
        let hir = match location.own_file {
            None => HirModule { id, items: vec![] },
            Some(file) => {
                let ast = self.ensure_ast(path, &file)?;
                let macros = self.macro_env(path).map_err(|e| {
                        self.modules
                            .failures
                            .insert(path.to_vec(), LoadFailure::Compile(e));
                        ResolveError::LoadFailed {
                            path: path.to_vec(),
                            message: "building macro environment failed".into(),
                        }
                    })?;
                let ast = omega_parser::macros::expand_with_origins(
                    (*ast).clone(),
                    &macros,
                    path,
                    &mut self.modules.macro_expansions,
                )
                .map_err(|e| {
                    self.modules
                        .failures
                        .insert(path.to_vec(), LoadFailure::MacroExpansion(e));
                    ResolveError::LoadFailed {
                        path: path.to_vec(),
                        message: "macro expansion failed".into(),
                    }
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
                index: None,
            },
        );
        Ok(hir)
    }

    pub(crate) fn load_failure(
        &mut self,
        module: &[Ident],
        error: ResolveError,
        importer: Option<ImportSite>,
    ) -> CompileError {
        match self.modules.take_failure(module) {
            Some(LoadFailure::Parse(errors)) => CompileError::Parse {
                module: module.to_vec(),
                errors,
            },
            Some(LoadFailure::MacroExpansion(error)) => CompileError::MacroExpansion {
                module: module.to_vec(),
                error,
            },
            Some(LoadFailure::Compile(error)) => error,
            None => CompileError::Resolve { error, importer },
        }
    }

    pub fn source_file(&self, module: &[Ident]) -> Option<Rc<SourceFile>> {
        self.modules.source(module)
    }

    pub(crate) fn ensure_module_indexed(&mut self, path: &[Ident]) -> Result<(), ResolveError> {
        if self.modules.is_indexed(path) {
            return Ok(());
        }
        let hir = self.parse_module(path)?;
        let (items, overloads) = self.index_items(path, &hir);
        // Published before imports are indexed: indexing an import resolves
        // its annotations, which runs an `Analyzer` that could ask this same
        // module for a name again -- marking it indexed here makes that
        // re-entry return immediately instead of recursing forever.
        self.modules.set_index(
            path,
            ModuleIndex {
                items,
                overloads,
                imports: IndexMap::new(),
            },
        );
        let imports = self.index_imports(path, &hir);
        self.modules.set_imports(path, imports);
        Ok(())
    }

    fn index_items(
        &mut self,
        path: &[Ident],
        hir: &HirModule,
    ) -> (IndexMap<Ident, usize>, IndexMap<Ident, Vec<usize>>) {
        let mut items: IndexMap<Ident, usize> = IndexMap::new();
        let mut overloads: IndexMap<Ident, Vec<usize>> = IndexMap::new();
        let is_function = |i: usize| matches!(&hir.items[i], HirItem::FunctionDefinition(_));

        for (i, item) in hir.items.iter().enumerate() {
            let Some(name) = item_name(item) else {
                continue;
            };
            let first_index = match items.entry(name.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(i);
                    continue;
                }
                Entry::Occupied(first) => *first.get(),
            };
            if is_function(i) && is_function(first_index) {
                overloads
                    .entry(name)
                    .or_insert_with(|| vec![first_index])
                    .push(i);
            } else {
                let (id, span) = item_id_span(item);
                let (_, previous) = item_id_span(&hir.items[first_index]);
                self.diagnostics.error(
                    path,
                    AnalysisError::new(
                        id,
                        span,
                        AnalysisErrorKind::Redeclaration {
                            name,
                            previous: Some(previous),
                        },
                    ),
                );
            }
        }
        (items, overloads)
    }

    fn index_imports(&mut self, path: &[Ident], hir: &HirModule) -> IndexMap<Ident, ImportEntry> {
        let mut imports: IndexMap<Ident, ImportEntry> = IndexMap::new();
        for item in &hir.items {
            let HirItem::Import(import) = item else {
                continue;
            };
            let alias = import
                .path
                .tail
                .last()
                .cloned()
                .unwrap_or_else(|| import.path.head.clone());
            let target = match self.import_absolute_path(path, import.root, &import.path) {
                Ok(target) => target,
                Err(e) => {
                    self.diagnostics.error(
                        path,
                        AnalysisError::new(
                            import.id,
                            import.span,
                            AnalysisErrorKind::ModuleResolution(e),
                        ),
                    );
                    continue;
                }
            };
            let annotations = import.annotations.clone();
            let suppress = self.analyze(
                path,
                &[],
                AnalysisSite::new(import.id, import.span),
                |analyzer| {
                    annotations::resolve(
                        analyzer,
                        import.id,
                        &annotations,
                        ItemKind::Import,
                        false,
                        false,
                    )
                    .suppress
                },
            );

            match imports.entry(alias) {
                Entry::Occupied(existing) => {
                    let previous = existing.get().span;
                    let name = existing.key().clone();
                    self.diagnostics.error(
                        path,
                        AnalysisError::new(
                            import.id,
                            import.span,
                            AnalysisErrorKind::Redeclaration {
                                name,
                                previous: Some(previous),
                            },
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

    pub(crate) fn local_item_index(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<usize, ResolveError> {
        self.ensure_module_indexed(module_path)?;
        self.modules
            .index(module_path)
            .items
            .get(name)
            .copied()
            .ok_or_else(|| ResolveError::UnknownItem {
                module: module_path.to_vec(),
                item: name.clone(),
            })
    }

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
            HirItem::Gap(_) => vec![],
            HirItem::Declaration { .. }
            | HirItem::DeclarationWithInit { .. }
            | HirItem::ExternDeclaration(_)
            | HirItem::Walrus { .. } => vec![],
            HirItem::Glue(_) | HirItem::Conform(_) | HirItem::Primitive(_) | HirItem::Import(_) => {
                unreachable!("unnamed items are never indexed into a module's items")
            }
        })
    }

    pub(crate) fn is_generic_template(
        &mut self,
        module_path: &[Ident],
        name: &Ident,
    ) -> Result<bool, ResolveError> {
        Ok(!self.item_generics(module_path, name)?.is_empty())
    }

    /// Converts an import's anchor and source path into one absolute
    /// `ModulePath`, shared by macro-import preparation and ordinary import
    /// indexing so both obey identical anchor semantics.
    fn import_absolute_path(
        &self,
        importer: &[Ident],
        root: ImportRoot,
        path: &Path,
    ) -> Result<ModulePath, ResolveError> {
        match root {
            ImportRoot::TopLevel => {
                if !self.roots.is_known_top_level(&path.head) {
                    return Err(ResolveError::UnknownTopLevelPackage(path.head.clone()));
                }
                Ok(path.segments())
            }
            ImportRoot::Root => {
                let mut absolute = Vec::new();
                if let Some(package_root) = importer.first() {
                    absolute.push(package_root.clone());
                }
                absolute.extend(path.segments());
                Ok(absolute)
            }
            ImportRoot::SelfModule => {
                let mut absolute = importer.to_vec();
                absolute.extend(path.segments());
                Ok(absolute)
            }
            ImportRoot::Super(depth) => {
                let depth = depth as usize;
                if depth >= importer.len() {
                    return Err(ResolveError::SuperAboveRoot {
                        depth: depth as u32,
                        importer: importer.to_vec(),
                    });
                }
                let mut absolute = importer[..importer.len() - depth].to_vec();
                absolute.extend(path.segments());
                Ok(absolute)
            }
        }
    }
}
