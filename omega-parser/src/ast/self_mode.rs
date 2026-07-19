/// How a member function receives `self` -- mirrors the four source
/// spellings `self` / `mut self` / `*self` / `*mut self`. Wrapped in
/// `Option` wherever it appears (`None` = an ordinary, non-member
/// function) so there is exactly one field carrying both "is this a
/// method" and "how does it receive self" -- no separate bool that could
/// disagree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfMode {
    /// `self` -- a by-value copy, immutable binding.
    Value,
    /// `mut self` -- a by-value copy, but the local copy is a mutable
    /// binding (reassignable/field-writable); never affects the caller.
    MutValue,
    /// `*self` -- a pointer to the caller's own value, immutable pointee.
    Pointer,
    /// `*mut self` -- a pointer to the caller's own value, mutable pointee.
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
