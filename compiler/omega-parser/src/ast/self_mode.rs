#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfMode {
    Value,
    MutValue,
    Pointer,
    MutPointer,
}

impl SelfMode {
    pub fn is_pointer(self) -> bool {
        matches!(self, Self::Pointer | Self::MutPointer)
    }

    pub fn is_mutable(self) -> bool {
        matches!(self, Self::MutValue | Self::MutPointer)
    }
}
