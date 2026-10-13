use crate::diagnostics::Span;

/// A declaration's receiver, with the span of its `self` token.
#[derive(Debug, Clone, Copy)]
pub struct SelfParam {
    pub mode: SelfMode,
    pub span: Span,
}

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
