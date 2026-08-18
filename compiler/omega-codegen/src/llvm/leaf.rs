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
        // Pointer-width integers and pointers share width but require distinct LLVM types.
        Leaf::Size => size_type(context, pointer_bytes).into(),
    }
}

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

pub(super) fn ptr_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    context.ptr_type(inkwell::AddressSpace::default())
}

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

pub(super) fn value_byte_width(ty: inkwell::types::BasicTypeEnum, pointer_bytes: u32) -> u32 {
    if ty.is_int_type() {
        ty.into_int_type().get_bit_width() / 8
    } else if ty.is_float_type() {
        match ty.into_float_type().get_bit_width() {
            32 => 4,
            _ => 8,
        }
    } else {
        // All semantic pointer leaves map to LLVM opaque pointers.
        pointer_bytes
    }
}
