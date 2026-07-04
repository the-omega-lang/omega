/// Identifies a single lowered module (one source file). Assigned by whoever
/// drives compilation (e.g. the `omgc` CLI) -- there is no hidden global
/// counter anywhere in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

/// Identifies a single HIR node, uniquely within its module. Minted only
/// during lowering (see [`crate::lower`]) -- nothing upstream (the parser) or
/// downstream (analysis, codegen) ever mints one of these itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId {
    pub module: ModuleId,
    pub local: u32,
}

/// A per-module id counter. Created fresh once per [`crate::lower::lower_module`]
/// call and threaded through lowering as a plain function argument -- no
/// thread-local or global state involved.
pub(crate) struct HirIdGen {
    module: ModuleId,
    next: u32,
}

impl HirIdGen {
    pub fn new(module: ModuleId) -> Self {
        Self { module, next: 0 }
    }

    pub fn next(&mut self) -> HirId {
        let local = self.next;
        self.next += 1;
        HirId {
            module: self.module,
            local,
        }
    }
}
