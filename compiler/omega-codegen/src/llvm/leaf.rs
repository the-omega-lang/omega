//! The LLVM-specific seam in the shared layout math (`omega_analyzer::
//! layout`): mapping a backend-agnostic [`Leaf`] onto LLVM's own types --
//! the exact counterpart of `cranelift/leaf.rs`'s `cranelift_type`, same
//! shape, same contract: never another copy of `omega_analyzer::layout`.

use inkwell::context::Context;
use inkwell::types::{BasicTypeEnum, PointerType};
use omega_analyzer::layout::{self, Leaf};
use omega_analyzer::resolved_type::ResolvedType;

pub(super) fn llvm_type<'ctx>(
    context: &'ctx Context,
    leaf: Leaf,
    pointer_bytes: u32,
) -> BasicTypeEnum<'ctx> {
    match leaf {
        Leaf::I8 => context.i8_type().into(),
        Leaf::I16 => context.i16_type().into(),
        Leaf::I32 => context.i32_type().into(),
        Leaf::I64 => context.i64_type().into(),
        Leaf::F32 => context.f32_type().into(),
        Leaf::F64 => context.f64_type().into(),
        Leaf::Ptr => ptr_type(context).into(),
        // The half of the old single `Ptr` leaf that is an *integer*
        // (`usize`/`isize`) rather than an address. Cranelift maps both to
        // the same type because its pointer type is an integer type; here
        // they must not be, or every size-typed value becomes an opaque
        // pointer and arithmetic on it is not even representable.
        Leaf::Size => size_type(context, pointer_bytes).into(),
    }
}

/// The pointer-width *integer* type -- `Leaf::Size`'s mapping, and the
/// type any `ptrtoint` in this backend produces.
pub(super) fn size_type<'ctx>(
    context: &'ctx Context,
    pointer_bytes: u32,
) -> inkwell::types::IntType<'ctx> {
    match pointer_bytes {
        8 => context.i64_type(),
        4 => context.i32_type(),
        2 => context.i16_type(),
        other => unreachable!("unsupported pointer width {other} bytes"),
    }
}

/// The one universal pointer type: LLVM's opaque pointer (i8*) -- every
/// address in this backend is carried as one, and every typed access
/// bitcasts or gep-then-loads through it.
pub(super) fn ptr_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    context.ptr_type(inkwell::AddressSpace::default())
}

/// `ResolvedType -> Vec<LLVM scalar types>` -- the shared layout math's
/// leaf list, mapped through `llvm_type`, exactly like
/// `cranelift/leaf.rs`'s `IntoCraneliftLeaves`.
pub(super) fn llvm_leaves<'ctx>(
    context: &'ctx Context,
    ty: &ResolvedType,
    pointer_bytes: u32,
) -> Vec<BasicTypeEnum<'ctx>> {
    layout::leaves_of(ty, pointer_bytes)
        .into_iter()
        .map(|leaf| llvm_type(context, leaf, pointer_bytes))
        .collect()
}

/// The byte width of one LLVM scalar value type -- the only per-leaf fact
/// `load_scalars`/`store_scalars` need from a value (the layout math
/// itself always comes from `omega_analyzer::layout::Leaf`).
pub(super) fn value_byte_width(ty: inkwell::types::BasicTypeEnum, pointer_bytes: u32) -> u32 {
    if ty.is_int_type() {
        ty.into_int_type().get_bit_width() / 8
    } else if ty.is_float_type() {
        match ty.into_float_type().get_bit_width() {
            32 => 4,
            _ => 8,
        }
    } else {
        // The opaque pointer type.
        pointer_bytes
    }
}
