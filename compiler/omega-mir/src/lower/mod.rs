mod function;
mod item;
mod place;

use crate::mir::MirModule;
use omega_analyzer::checked::CheckedModule;
use omega_hir::ModulePath;

pub fn lower_program(
    modules: Vec<(ModulePath, CheckedModule)>,
    entry: &ModulePath,
) -> Vec<(ModulePath, MirModule)> {
    modules
        .into_iter()
        .map(|(path, module)| {
            let module = item::lower_module(module, &path, entry);
            (path, module)
        })
        .collect()
}
