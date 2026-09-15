mod aliases;
mod bodies;
pub(crate) mod compile;
mod conformances;
mod diagnostics;
mod error;
mod fs_resolve;
mod items;
mod modules;
mod primitives;
mod resolver;
mod roots;

pub use error::{CompileError, CompiledProgram};
pub use fs_resolve::basename;
pub use roots::{ExternRoot, LocalSource};

use aliases::AliasState;
use conformances::Conformances;
use diagnostics::Diagnostics;
use items::ItemQueries;
use modules::ModuleStore;
use omega_analyzer::Target;
use omega_analyzer::compiler_definitions::CompilerDefinitions;
use omega_parser::prelude::Ident;
use omega_parser::prelude::MacroDefinitionStmt;
use primitives::Primitives;
use resolver::ImportState;
use roots::ModuleRoots;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub(crate) type ModulePath = Vec<Ident>;

pub struct Driver {
    roots: ModuleRoots,
    modules: ModuleStore,
    diagnostics: Diagnostics,
    items: ItemQueries,
    imports: ImportState,
    aliases: AliasState,
    primitives: Primitives,
    conformances: Conformances,
    prelude_macros: Option<Rc<HashMap<Ident, MacroDefinitionStmt>>>,
    /// Active analyzer runs, innermost last. A nested run started for a
    /// concrete generic instantiation reads the enclosing entry to name the
    /// use that demanded it.
    analysis_stack: Vec<(ModulePath, omega_analyzer::analysis::AnalysisSite)>,
    /// The target and compiler definitions every source of this compilation
    /// is read with. It is fixed at construction: a cache populated under one
    /// configuration would be wrong under any other.
    definitions: CompilerDefinitions,
}

impl Driver {
    /// A driver with no compiler definitions, for a compilation that selects
    /// nothing beyond its target.
    pub fn new(
        root: PathBuf,
        root_name: Option<Ident>,
        externs: Vec<ExternRoot>,
        target: Target,
    ) -> Result<Self, Vec<CompileError>> {
        Self::new_with_definitions(root, root_name, externs, CompilerDefinitions::new(target))
    }

    pub fn new_with_definitions(
        root: PathBuf,
        root_name: Option<Ident>,
        externs: Vec<ExternRoot>,
        definitions: CompilerDefinitions,
    ) -> Result<Self, Vec<CompileError>> {
        Ok(Self {
            roots: ModuleRoots::new(root, root_name, externs)?,
            modules: ModuleStore::default(),
            diagnostics: Diagnostics::default(),
            items: ItemQueries::default(),
            imports: ImportState::default(),
            aliases: AliasState::default(),
            primitives: Primitives::default(),
            conformances: Conformances::default(),
            prelude_macros: None,
            analysis_stack: Vec::new(),
            definitions,
        })
    }

    pub(crate) fn target(&self) -> Target {
        self.definitions.target()
    }
}
