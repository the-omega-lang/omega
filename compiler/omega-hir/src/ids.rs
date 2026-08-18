#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId {
    pub module: ModuleId,
    pub local: u32,
}

pub const SYNTHETIC_MODULE: ModuleId = ModuleId(u32::MAX);

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
