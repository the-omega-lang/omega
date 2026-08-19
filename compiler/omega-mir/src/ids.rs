#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    pub(crate) fn from_index(index: usize) -> Self {
        Self(
            u32::try_from(index)
                .expect("omega-mir cannot represent more than u32::MAX blocks in one function"),
        )
    }

    pub fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 MIR block IDs must fit in usize on supported hosts")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

impl LocalId {
    pub(crate) fn from_index(index: usize) -> Self {
        Self(
            u32::try_from(index)
                .expect("omega-mir cannot represent more than u32::MAX locals in one function"),
        )
    }

    pub fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 MIR local IDs must fit in usize on supported hosts")
    }
}
