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
pub use roots::ExternRoot;

use conformances::Conformances;
use diagnostics::Diagnostics;
use items::ItemQueries;
use modules::ModuleStore;
use omega_analyzer::Target;
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
    primitives: Primitives,
    conformances: Conformances,
    prelude_macros: Option<Rc<HashMap<Ident, MacroDefinitionStmt>>>,
    target: Target,
}

impl Driver {
    pub fn new(
        root: PathBuf,
        root_name: Option<Ident>,
        externs: Vec<ExternRoot>,
        target: Target,
    ) -> Result<Self, Vec<CompileError>> {
        Ok(Self {
            roots: ModuleRoots::new(root, root_name, externs)?,
            modules: ModuleStore::default(),
            diagnostics: Diagnostics::default(),
            items: ItemQueries::default(),
            imports: ImportState::default(),
            primitives: Primitives::default(),
            conformances: Conformances::default(),
            prelude_macros: None,
            target,
        })
    }
}
