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
use omega_parser::prelude::{
    AliasItem, AliasTarget, Ident, ImportStmt, Item, ParseError, Path, PathAnchor, SourceModule,
};
use omega_parser::prelude::{MacroDefinitionStmt, Visibility};
use std::collections::{HashMap, HashSet};
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
    /// Declared aliases, kept apart from `items` because an alias is a name
    /// without a declaration: it must never reach item resolution, emission,
    /// or any other concrete-item sweep.
    pub aliases: IndexMap<Ident, usize>,
    /// Item indices of the imports that won their name in the shared
    /// module-scope claim pass. Import *targets* resolve in a later phase, so
    /// the names they bind are settled here and consulted there.
    claimed_imports: Vec<usize>,
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
    /// Which of a module's macro bindings came from an `alias` rather than a
    /// `macro` definition. An ordinary type/function alias never appears here,
    /// so the macro namespace stays separate from the ordinary one.
    macro_alias_names: HashMap<ModulePath, Rc<HashSet<Ident>>>,
    macro_expansions: omega_parser::macros::ExpansionState,
    sources: HashMap<ModulePath, Rc<SourceFile>>,
    failures: HashMap<ModulePath, LoadFailure>,
    definition_origins: HashMap<ModulePath, omega_parser::prelude::Origin>,
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

    /// One shared origin per module naming that module as the place syntax
    /// carrying it must resolve in. Alias expansion stamps it onto a target
    /// type so the target keeps resolving at its declaration site.
    pub fn definition_origin(&mut self, module: &[Ident]) -> omega_parser::prelude::Origin {
        if let Some(origin) = self.definition_origins.get(module) {
            return *origin;
        }
        let origin = self.macro_expansions.register_definition_module(module);
        self.definition_origins.insert(module.to_vec(), origin);
        origin
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
                        if let Err(error) =
                            omega_parser::macros::bind_definition(&mut definition, path)
                        {
                            self.modules
                                .failures
                                .insert(path.to_vec(), LoadFailure::MacroExpansion(error));
                            return Err(ResolveError::LoadFailed {
                                path: path.to_vec(),
                                message: "macro expansion failed".into(),
                            });
                        }
                        definitions.insert(definition.name.clone(), definition);
                    }
                }
                let aliased = self.bind_macro_aliases(path, &ast, &mut definitions);
                self.modules
                    .macro_alias_names
                    .insert(path.to_vec(), Rc::new(aliased));
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

    /// The names this module binds in the macro namespace through an `alias`.
    /// Only meaningful after `module_macros` has run for `path`.
    fn macro_alias_names(&self, path: &[Ident]) -> Rc<HashSet<Ident>> {
        self.modules
            .macro_alias_names
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// Binds macro names reached through an `alias`. This runs before macro
    /// expansion, so it works from raw AST rather than from the alias query,
    /// which cannot exist yet: indexing a module requires its HIR, and its HIR
    /// requires this environment. The expansion keeps the target macro's body
    /// and defining module for hygiene, but takes the alias's name and
    /// visibility, which gate who may invoke the alias -- never what the
    /// body may reach.
    fn bind_macro_aliases(
        &mut self,
        path: &[Ident],
        ast: &SourceModule,
        definitions: &mut HashMap<Ident, MacroDefinitionStmt>,
    ) -> HashSet<Ident> {
        let mut bound: HashSet<Ident> = HashSet::new();
        let declared: Vec<AliasItem> = ast
            .nodes
            .iter()
            .filter_map(|node| match &node.item {
                Item::Alias(declared) if declared.generics.is_empty() => Some(declared.clone()),
                _ => None,
            })
            .collect();
        if declared.is_empty() {
            return bound;
        }
        let imports: Vec<ImportStmt> = ast
            .nodes
            .iter()
            .filter_map(|node| match &node.item {
                Item::Import(import) => Some(import.clone()),
                _ => None,
            })
            .collect();

        // A chain may be written in any order, so binding repeats until no
        // further alias resolves. Each round binds at least one name, which
        // bounds the loop by the number of aliases.
        let mut remaining = declared;
        loop {
            let mut bound_any = false;
            let mut deferred = Vec::with_capacity(remaining.len());
            for declared in remaining {
                let AliasTarget::Path(target) = &declared.target else {
                    continue;
                };
                match self.raw_macro_target(path, &imports, target, definitions) {
                    Some(definition) => {
                        definitions.insert(
                            declared.ident.clone(),
                            MacroDefinitionStmt {
                                name: declared.ident.clone(),
                                visibility: declared.visibility,
                                ..definition
                            },
                        );
                        bound.insert(declared.ident);
                        bound_any = true;
                    }
                    None => deferred.push(declared),
                }
            }
            remaining = deferred;
            if !bound_any || remaining.is_empty() {
                return bound;
            }
        }
    }

    /// Resolves a macro alias's raw target, rejecting a target macro binding
    /// not visible from `path` -- the alias declaration module. An anchored
    /// target (`root::`/`self::`/`super::`) goes through the same shared
    /// anchor-resolution machinery ordinary paths and imports use, rather
    /// than reimplementing it here.
    fn raw_macro_target(
        &mut self,
        path: &[Ident],
        imports: &[ImportStmt],
        target: &Path,
        local: &HashMap<Ident, MacroDefinitionStmt>,
    ) -> Option<MacroDefinitionStmt> {
        if let Some(anchored) = Driver::resolve_explicit_anchor(path, target) {
            let absolute = anchored.ok()?;
            let (name, module) = absolute.split_last()?;
            return self.gate_macro_target(path, module, name, local);
        }
        if target.is_unqualified() {
            if let Some(definition) = local.get(&target.head) {
                return Some(definition.clone());
            }
            // An import binds the same name a local definition would, so a
            // bare target may name an imported macro. The import's own
            // `reveal` decides whether that binding is usable here, exactly
            // as it does when the macro is invoked directly instead.
            let import = Self::binding_import(imports, &target.head)?;
            let absolute = self.import_absolute_path(path, &import.path).ok()?;
            let (name, module) = absolute.split_last()?;
            let (name, module) = (name.clone(), module.to_vec());
            if import.reveal {
                return self.module_macros(&module).ok()?.get(&name).cloned();
            }
            return self.gate_macro_target(path, &module, &name, local);
        }
        let (name, module) = target
            .segments()
            .split_last()
            .map(|(n, m)| (n.clone(), m.to_vec()))?;
        let base = Self::binding_import(imports, &target.head)
            .and_then(|import| self.import_absolute_path(path, &import.path).ok())
            .or_else(|| {
                self.roots
                    .is_known_top_level(&target.head)
                    .then(|| vec![target.head.clone()])
            })?;
        let absolute: Vec<Ident> = base.into_iter().chain(module.into_iter().skip(1)).collect();
        self.gate_macro_target(path, &absolute, &name, local)
    }

    /// The import that binds `name` in the importing module, if any.
    fn binding_import<'i>(imports: &'i [ImportStmt], name: &Ident) -> Option<&'i ImportStmt> {
        imports
            .iter()
            .find(|import| import.path.tail.last().unwrap_or(&import.path.head) == name)
    }

    /// The named macro binding in `module`, visible from `accessor`, or
    /// `None` if it does not exist or is not visible from there. A hidden or
    /// shared intermediate binding may not be smuggled through an alias any
    /// more than a hidden item may.
    fn gate_macro_target(
        &mut self,
        accessor: &[Ident],
        module: &[Ident],
        name: &Ident,
        local: &HashMap<Ident, MacroDefinitionStmt>,
    ) -> Option<MacroDefinitionStmt> {
        let definition = if module == accessor {
            local.get(name).cloned()?
        } else {
            self.module_macros(module).ok()?.get(name).cloned()?
        };
        Self::visibility_allows(definition.visibility, module, accessor).then_some(definition)
    }

    pub(crate) fn module_has_macro(&mut self, module: &[Ident], name: &Ident) -> bool {
        self.module_macros(module)
            .is_ok_and(|definitions| definitions.contains_key(name))
    }

    pub(crate) fn macro_visibility(
        &mut self,
        module: &[Ident],
        name: &Ident,
    ) -> Option<Visibility> {
        Some(self.module_macros(module).ok()?.get(name)?.visibility)
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

    /// The macro environment a module's own source expands in: the ambient
    /// exposed `core` macros, overlaid with this module's explicit bindings.
    ///
    /// Ambient entries are fallback names, so a definition, macro alias, or
    /// macro import replaces one. Two explicit bindings of the same name are a
    /// collision instead, which is why the three forms are collected in one
    /// source-order pass rather than layered by insertion order.
    fn macro_env(
        &mut self,
        path: &[Ident],
    ) -> Result<HashMap<Ident, MacroDefinitionStmt>, CompileError> {
        let resolve_failed = |error| CompileError::Resolve {
            error,
            importer: None,
        };
        let mut environment = (*self.prelude_macros()?).clone();
        let own = self.module_macros(path).map_err(resolve_failed)?;
        if path.first().map(Ident::as_ref) == Some("core") {
            for name in own.keys() {
                environment.remove(name);
            }
        }

        let location = self.roots.locate(path).map_err(resolve_failed)?;
        let Some(file) = location.own_file else {
            return Ok(environment);
        };
        let ast = self.ensure_ast(path, &file).map_err(resolve_failed)?;
        let aliased = self.macro_alias_names(path);

        let mut explicit: HashMap<Ident, Span> = HashMap::new();
        for node in &ast.nodes {
            let bound = match &node.item {
                Item::MacroDefinition(definition) => own.get(&definition.name).cloned(),
                // Only an alias whose target actually resolved to a macro
                // binds in this namespace; an ordinary type/function alias
                // that happens to share a spelling with a macro does not.
                Item::Alias(declared) if aliased.contains(&declared.ident) => {
                    own.get(&declared.ident).cloned()
                }
                Item::Import(import) => self.imported_macro(path, import),
                _ => continue,
            };
            let Some(definition) = bound else {
                continue;
            };
            let name = definition.name.clone();
            if let Some(previous) = explicit.insert(name.clone(), node.span) {
                return Err(CompileError::MacroNameCollision {
                    module: path.to_vec(),
                    name,
                    previous,
                    span: node.span,
                });
            }
            environment.insert(name, definition);
        }
        Ok(environment)
    }

    /// The macro `import` binds in `path`, if it binds one at all.
    fn imported_macro(
        &mut self,
        path: &[Ident],
        import: &ImportStmt,
    ) -> Option<MacroDefinitionStmt> {
        let absolute = self.import_absolute_path(path, &import.path).ok()?;
        if self.roots.module_exists(&absolute) || absolute.len() < 2 {
            return None;
        }
        let module = absolute[..absolute.len() - 1].to_vec();
        let name = absolute.last().expect("non-empty import path").clone();
        if !self.roots.module_exists(&module) {
            return None;
        }
        let definition = self.module_macros(&module).ok()?.get(&name)?.clone();
        // The same rule the alias-target lookup applies, so `reveal` cannot
        // mean one thing when the macro is invoked and another when it is
        // immediately aliased.
        (import.reveal || Self::visibility_allows(definition.visibility, &module, path))
            .then_some(definition)
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
                let source = self.modules.source(path);
                let ast = omega_parser::macros::expand_with_origins(
                    (*ast).clone(),
                    &macros,
                    path,
                    source.as_deref(),
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
        let index = self.index_items(path, &hir);
        // Published before imports are indexed: indexing an import resolves
        // its annotations, which runs an `Analyzer` that could ask this same
        // module for a name again -- marking it indexed here makes that
        // re-entry return immediately instead of recursing forever.
        self.modules.set_index(path, index);
        let imports = self.index_imports(path, &hir);
        self.modules.set_imports(path, imports);
        Ok(())
    }

    /// The name an item claims in the module's ordinary namespace, or `None`
    /// for an item that binds no such name (`conform`, `glue`, `primitive`).
    fn claimed_name(item: &HirItem) -> Option<Ident> {
        match item {
            HirItem::Alias(declared) => Some(declared.name.clone()),
            HirItem::Import(import) => Some(
                import
                    .path
                    .tail
                    .last()
                    .cloned()
                    .unwrap_or_else(|| import.path.head.clone()),
            ),
            _ => item_name(item),
        }
    }

    /// Indexes a module's declarations, aliases, and import bindings in one
    /// source-order pass. Declarations, aliases, and imports all make an
    /// *explicit* claim on the same module-scope namespace, so two claims on a
    /// name collide whichever forms they take; function overloads are the one
    /// deliberate exception. Scanning them together is what keeps the reported
    /// collision the later claim regardless of which kind came first.
    fn index_items(&mut self, path: &[Ident], hir: &HirModule) -> ModuleIndex {
        let mut items: IndexMap<Ident, usize> = IndexMap::new();
        let mut overloads: IndexMap<Ident, Vec<usize>> = IndexMap::new();
        let mut aliases: IndexMap<Ident, usize> = IndexMap::new();
        let mut claimed_imports: Vec<usize> = Vec::new();
        let mut claims: IndexMap<Ident, usize> = IndexMap::new();
        let is_function = |i: usize| matches!(&hir.items[i], HirItem::FunctionDefinition(_));

        for (i, item) in hir.items.iter().enumerate() {
            let Some(name) = Self::claimed_name(item) else {
                continue;
            };
            if omega_analyzer::is_reserved_type_name(name.as_ref()) {
                let (id, span) = item_id_span(item);
                self.diagnostics.error(
                    path,
                    AnalysisError::new(id, span, AnalysisErrorKind::ReservedTypeName { name }),
                );
                continue;
            }
            let first_index = match claims.entry(name.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(i);
                    match item {
                        HirItem::Alias(_) => {
                            aliases.insert(name, i);
                        }
                        HirItem::Import(_) => claimed_imports.push(i),
                        _ => {
                            items.insert(name, i);
                        }
                    }
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
                let (_, previous) = item_id_span(&hir.items[first_index]);
                self.report_redeclaration(path, item, name, previous);
            }
        }

        ModuleIndex {
            items,
            overloads,
            imports: IndexMap::new(),
            aliases,
            claimed_imports,
        }
    }

    fn report_redeclaration(
        &mut self,
        path: &[Ident],
        item: &HirItem,
        name: Ident,
        previous: Span,
    ) {
        let (id, span) = item_id_span(item);
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

    /// Resolves the targets of the imports that won their name in
    /// `index_items`. Only their targets are resolved here: an import that
    /// lost its name binds nothing, and one whose target fails to resolve
    /// still keeps the name it claimed.
    fn index_imports(&mut self, path: &[Ident], hir: &HirModule) -> IndexMap<Ident, ImportEntry> {
        let mut imports: IndexMap<Ident, ImportEntry> = IndexMap::new();
        let claimed = self.modules.index(path).claimed_imports.clone();
        for i in claimed {
            let HirItem::Import(import) = &hir.items[i] else {
                unreachable!("the claimed-import list only ever points at imports");
            };
            let alias = import
                .path
                .tail
                .last()
                .cloned()
                .unwrap_or_else(|| import.path.head.clone());
            let target = match self.import_absolute_path(path, &import.path) {
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
                        annotations::ManglingMode::Enabled,
                    )
                    .suppress
                },
            );

            imports.insert(
                alias,
                ImportEntry {
                    id: import.id,
                    span: import.span,
                    target,
                    suppress,
                    reveal: import.reveal,
                },
            );
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
        // A static-spec parameter contributes an anonymous bounded generic, so
        // a function's generic arity is only known after normalization.
        if let HirItem::FunctionDefinition(f) = &self.modules.parsed(module_path).hir.items[index] {
            let f = f.clone();
            return Ok(self.normalized_function(module_path, &f)?.generics);
        }
        Ok(match &self.modules.parsed(module_path).hir.items[index] {
            HirItem::Struct(s) => s.generics.clone(),
            HirItem::Enum(e) => e.generics.clone(),
            HirItem::Union(u) => u.generics.clone(),
            HirItem::FunctionDefinition(f) => f.generics.clone(),
            HirItem::ForeignFunction(f) => f.generics.clone(),
            HirItem::Spec(sp) => sp.generics.clone(),
            HirItem::Gap(_) => vec![],
            HirItem::Declaration { .. }
            | HirItem::DeclarationWithInit { .. }
            | HirItem::ForeignBinding(_)
            | HirItem::Walrus { .. } => vec![],
            HirItem::Glue(_)
            | HirItem::Conform(_)
            | HirItem::Primitive(_)
            | HirItem::Import(_)
            | HirItem::Alias(_) => {
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

    /// Converts an import's path (including its optional explicit anchor)
    /// into one absolute `ModulePath`. An unanchored import is
    /// top-level-by-default.
    fn import_absolute_path(
        &self,
        importer: &[Ident],
        path: &Path,
    ) -> Result<ModulePath, ResolveError> {
        match Self::resolve_explicit_anchor(importer, path) {
            Some(result) => result,
            None => {
                if !self.roots.is_known_top_level(&path.head) {
                    return Err(ResolveError::UnknownTopLevelPackage(path.head.clone()));
                }
                Ok(path.segments())
            }
        }
    }

    /// Resolves a `Path`'s explicit anchor (`root::`/`self::`/`super::`)
    /// relative to `origin_module` -- the path's semantic resolution module:
    /// the importer for imports, the macro-origin module for macro-expanded
    /// paths, the alias declaration module for aliases. Returns `None` when
    /// the path carries no explicit anchor, so callers fall back to their own
    /// (import-default-top-level vs. ordinary-relative) unanchored lookup.
    pub(crate) fn resolve_explicit_anchor(
        origin_module: &[Ident],
        path: &Path,
    ) -> Option<Result<ModulePath, ResolveError>> {
        Some(match path.anchor? {
            PathAnchor::Root => {
                let mut absolute = Vec::new();
                if let Some(package_root) = origin_module.first() {
                    absolute.push(package_root.clone());
                }
                absolute.extend(path.segments());
                Ok(absolute)
            }
            PathAnchor::SelfModule => {
                let mut absolute = origin_module.to_vec();
                absolute.extend(path.segments());
                Ok(absolute)
            }
            PathAnchor::Super(depth) => {
                let depth = depth as usize;
                if depth >= origin_module.len() {
                    Err(ResolveError::SuperAboveRoot {
                        depth: depth as u32,
                        importer: origin_module.to_vec(),
                    })
                } else {
                    let mut absolute = origin_module[..origin_module.len() - depth].to_vec();
                    absolute.extend(path.segments());
                    Ok(absolute)
                }
            }
        })
    }
}
