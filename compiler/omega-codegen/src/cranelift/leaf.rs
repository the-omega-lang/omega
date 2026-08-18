use super::Codegen;
use cranelift::prelude::{Type, types};
use omega_analyzer::layout::{self, Leaf};
use omega_analyzer::resolved_type::ResolvedType;

pub(super) fn cranelift_type(leaf: Leaf, pointer_type: Type) -> Type {
    match leaf {
        Leaf::I8 => types::I8,
        Leaf::I16 => types::I16,
        Leaf::I32 => types::I32,
        Leaf::I64 => types::I64,
        Leaf::F32 => types::F32,
        Leaf::F64 => types::F64,
        // Cranelift models pointers as integer-typed values, so semantic pointer identity must remain explicit.
        Leaf::Ptr | Leaf::Size => pointer_type,
    }
}

pub(super) trait IntoCraneliftLeaves {
    fn cranelift_leaves(self, codegen: &Codegen) -> Vec<Type>;
}

impl IntoCraneliftLeaves for &ResolvedType {
    fn cranelift_leaves(self, codegen: &Codegen) -> Vec<Type> {
        layout::leaves_of(self, codegen.pointer_bytes())
            .into_iter()
            .map(|leaf| cranelift_type(leaf, codegen.pointer_type()))
            .collect()
    }
}
