mod function;
mod item;
mod place;

use crate::mir::MirModule;
use omega_analyzer::checked::CheckedModule;
use omega_parser::prelude::Ident;

pub fn lower_program(
    modules: Vec<(Vec<Ident>, CheckedModule)>,
    entry: &[Ident],
) -> Vec<(Vec<Ident>, MirModule)> {
    modules
        .into_iter()
        .map(|(path, module)| {
            let module = item::lower_module(module, &path, entry);
            (path, module)
        })
        .collect()
}
