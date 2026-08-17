//! The one Cranelift-specific seam in the shared layout math
//! (`omega_analyzer::layout`): mapping a backend-agnostic [`Leaf`] onto
//! Cranelift's own `Type`. A future backend adds an equally small mapping
//! of its own here-shaped module -- never another copy of `omega_analyzer::layout`.

use super::Codegen;
use omega_analyzer::layout::{self, Leaf};
use cranelift::prelude::{Type, types};
use omega_analyzer::resolved_type::ResolvedType;

pub(super) fn cranelift_type(leaf: Leaf, pointer_type: Type) -> Type {
    match leaf {
        Leaf::I8 => types::I8,
        Leaf::I16 => types::I16,
        Leaf::I32 => types::I32,
        Leaf::I64 => types::I64,
        Leaf::F32 => types::F32,
        Leaf::F64 => types::F64,
        // Cranelift's `pointer_type()` is an ordinary integer type, so an
        // address and a pointer-width integer are genuinely the same thing
        // here -- the distinction `Leaf` draws (see its doc comment) exists
        // for backends whose type systems separate the two, and collapsing
        // it back is the correct mapping for this one, not a shortcut.
        Leaf::Ptr | Leaf::Size => pointer_type,
    }
}

/// `ResolvedType -> Vec<cranelift::Type>` -- the direct replacement for
/// what used to be a self-contained `IntoIRType` trait computing
/// Cranelift types directly; the actual layout math now lives in
/// `omega_analyzer::layout`, shared with any future backend, and this is just the
/// last step (`Leaf` -> `Type`) on top of it. Implemented for
/// `&ResolvedType` (borrowing) rather than by value, so call sites never
/// need a defensive `.clone()` just to ask "how many leaves does this
/// have" -- method resolution auto-refs through a `Box<ResolvedType>`
/// (e.g. a function's own boxed `return_type`) the same way it would for
/// any other borrowed-receiver trait method.
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
