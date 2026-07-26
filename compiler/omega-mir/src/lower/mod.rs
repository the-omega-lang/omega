//! The `CheckedModule -> MirModule` lowering pass -- see the crate root doc
//! comment for the pipeline this sits in.

mod function;
mod item;
mod place;

use crate::mir::MirModule;
use omega_analyzer::checked::CheckedModule;
use omega_parser::prelude::Ident;

/// Lowers every checked module a compilation produced into its MIR
/// counterpart, one-to-one, in the same order. Each module lowers
/// independently -- monomorphization has already fully run by the time a
/// `CheckedModule` exists (see `omega_driver::compile`), so there is no
/// whole-program state to thread through here.
pub fn lower_program(modules: Vec<(Vec<Ident>, CheckedModule)>) -> Vec<(Vec<Ident>, MirModule)> {
    modules.into_iter().map(|(path, module)| (path, item::lower_module(module))).collect()
}
