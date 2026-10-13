mod function;
mod item;
mod place;

use crate::mir::MirModule;
use omega_analyzer::checked::CheckedModule;
use omega_analyzer::type_store::TypeStore;
use omega_hir::ModulePath;

pub fn lower_program(
    modules: Vec<(ModulePath, CheckedModule)>,
    entry: &ModulePath,
    types: &TypeStore,
) -> Vec<(ModulePath, MirModule)> {
    modules
        .into_iter()
        .map(|(path, module)| {
            let module = item::lower_module(types, module, &path, entry);
            (path, module)
        })
        .collect()
}
