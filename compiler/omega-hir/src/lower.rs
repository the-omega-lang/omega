mod expression;
mod item;
mod statement;
#[cfg(test)]
mod tests;

use crate::hir::HirModule;
use crate::ids::{HirIdGen, ModuleId};
use omega_parser::prelude::SourceModule;

pub fn lower_module(module: ModuleId, ast: &SourceModule) -> HirModule {
    let mut lowerer = Lowerer {
        ids: HirIdGen::new(module),
    };
    let items = ast
        .nodes
        .iter()
        .flat_map(|node| lowerer.lower_item(node))
        .collect();
    HirModule { id: module, items }
}

struct Lowerer {
    ids: HirIdGen,
}
