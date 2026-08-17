//! Drives a whole compilation: finds modules on disk, parses each at most
//! once, and answers every module-shaped question `omega-analyzer` asks while
//! it checks them.
//!
//! Everything here is organized around one idea: **every top-level item is
//! its own independent, memoized query**. There is no per-module signature
//! unit, and no architectural difference between a same-module reference, a
//! cross-module one, and a generic instantiation -- all three are the same
//! query with a different key ([`items::ItemKey`]). That is what makes
//! declaration order irrelevant everywhere, keeps one bad item from poisoning
//! an unrelated sibling, and lets a by-value type cycle be rejected exactly at
//! the item that closes it.
//!
//! The pieces, in dependency order:
//!
//! - [`roots`]/[`fs_resolve`] -- the only place a module path becomes a
//!   filesystem lookup.
//! - [`modules`] -- parsing, indexing, and walking the import graph.
//! - [`diagnostics`] -- where findings accumulate, and the one way a
//!   throwaway per-item `Analyzer` is ever run.
//! - [`items`] -- the item query itself (phase 1: signatures).
//! - [`bodies`] -- phase 2, reading phase 1's results back.
//! - [`compile`] -- the two-phase whole-program sweep.
//! - [`resolver`] -- the `ModuleResolver` implementation the analyzer sees.

mod bodies;
pub(crate) mod compile;
mod conformances;
mod diagnostics;
mod error;
mod fs_resolve;
mod items;
mod modules;
mod resolver;
mod roots;

pub use error::{CompileError, CompiledProgram};
pub use fs_resolve::basename;
pub use roots::ExternRoot;

use conformances::Conformances;
use conformances::Primitives;
use diagnostics::Diagnostics;
use items::ItemQueries;
use modules::ModuleStore;
use omega_parser::ast::statement::macro_definition::MacroDefinitionStmt;
use omega_analyzer::Target;
use omega_parser::prelude::Ident;
use resolver::ImportState;
use roots::ModuleRoots;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// An absolute module path, from its package root down (`["core",
/// "strings"]`). Every cache, diagnostic, and mangled symbol keys off the
/// *declared* path -- never off wherever its content happens to live on disk.
pub(crate) type ModulePath = Vec<Ident>;

/// Owns everything module-tree-shaped for one compilation.
///
/// The state is deliberately split by concern rather than kept as one flat
/// bag: each field below is a self-contained cache with its own invariants,
/// and nothing reaches across them except the methods that genuinely
/// coordinate two phases.
pub struct Driver {
    /// Where module content comes from on disk.
    roots: ModuleRoots,
    /// Every module parsed so far, plus its lazily built name index.
    modules: ModuleStore,
    /// Every finding, bucketed by module, drained once at the end.
    diagnostics: Diagnostics,
    /// The item-granular query cache: signatures, type cells, and the checked
    /// bodies of everything `compile`'s static sweep can't enumerate.
    items: ItemQueries,
    /// Import aliases: what each resolves to, and which were ever used.
    imports: ImportState,
    /// Primitive and conform declarations, which sit outside named-item queries.
    primitives: Primitives,
    conformances: Conformances,
    /// Every exposed macro in the ambient `core` prelude, collected once.
    prelude_macros: Option<Rc<HashMap<Ident, MacroDefinitionStmt>>>,
    /// The compilation target -- see `Driver::new`'s doc comment.
    target: Target,
}

impl Driver {
    /// `root` is the local project's own root directory, eagerly and fully
    /// discovered right here (see `roots::ModuleRoots::new`); `root_name`
    /// is an *explicit* `--name=` override, `None` when `omgc` wasn't given
    /// one (see `ModuleRoots::new`'s doc comment for why that distinction,
    /// not a plain already-defaulted name, is what this needs); `externs`
    /// is every `--extern` the CLI was given. Fails if two package roots
    /// claim the same declared name.
    /// `target` is the compilation target every piece of analysis runs
    /// under -- carried here so every `Analyzer` constructed along the way
    /// resolves width-sensitive questions (`numeric_kind`'s
    /// `ISize`/`USize`, `integer_domain`, `comp`'s `sizeof`) against the
    /// same real target `omgc` was given. `Driver::compile` re-sets it per
    /// run, so one driver can compile for different targets in sequence.
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
