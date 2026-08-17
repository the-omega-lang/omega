//! Evaluating one [`MirExprNode`] into its scalar leaves -- the LLVM
//! counterpart of `cranelift/expr.rs`, line for line in *semantics* (the
//! shared layout math, the shared ABI, and the MIR-carried alignment do
//! the deciding; this module only translates).

use super::leaf;
use super::place::PlaceStorage;
use super::Codegen;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, GlobalValue, PointerValue, ValueKind};
use omega_analyzer::checked::{CastKind, NumberValue};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ConstValue, NumericKind, ResolvedType};
use omega_hir::BinaryOp;
use omega_mir::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall, MirEnumConstruct,
    MirExpr, MirExprNode, MirFunctionCall, MirSlice, MirSpecCoerce, MirStructLiteral, MirUnionConstruct,
};

/// A `ConstValue::Ref(inner)`'s own real type, given the *pointee* type its
/// enclosing `ResolvedType::Pointer` carries -- the exact counterpart of
/// `cranelift/expr.rs`'s `ref_pointee_type` (see there for the
/// `DropLength`-produced `Array` shape it exists for).
fn ref_pointee_type(inner: &ConstValue, leaf_type: &ResolvedType) -> ResolvedType {
    match inner {
        ConstValue::Array(elements) => {
            ResolvedType::SizedArray(Box::new(leaf_type.clone()), elements.len() as u32)
        }
        _ => leaf_type.clone(),
    }
}

/// A constant's physical byte image plus its pointer relocations -- the
/// exact counterpart of Cranelift's `DataDescription` +
/// `write_const_element` pair, kept backend-native: the bytes mirror
/// `cranelift/expr.rs`'s buffer exactly (same layout math, same offsets,
/// same little-endian writes), and the relocations are `(byte offset,
/// target global)` pairs LLVM turns into packed-struct initializer fields.
pub(super) struct ConstBlob<'ctx> {
    pub bytes: Vec<u8>,
    pub relocs: Vec<(u32, GlobalValue<'ctx>)>,
    pub pointer_bytes: u32,
}

impl<'ctx> Codegen<'ctx> {
    /// `process_expr`'s LLVM counterpart: one `MirExprNode` into its
    /// scalar leaves.
    pub(super) fn process_expr(&mut self, node: &MirExprNode) -> Vec<BasicValueEnum<'ctx>> {
        match &node.kind {
            MirExpr::String(s) | MirExpr::ByteString(s) => self.emit_bytes(s.clone()),
            MirExpr::Const(value) => self.emit_const_value(value, &node.r#type),

            MirExpr::FunctionCall(MirFunctionCall { callee, fn_type, args }) => {
                // The callee is evaluated to a single address and called
                // indirectly, exactly as `cranelift/expr.rs` does -- *not*
                // resolved to a declared `FunctionValue`. A callee is not
                // always a direct function place: a function *pointer*
                // (`ResolvedType::Function` held in a local, a field, or a
                // parameter) is an ordinary value here, and insisting on a
                // `MirPlaceRoot::Function` root rejects every such call.
                // Nothing is lost on the direct case either -- LLVM prints a
                // call through a constant function address as an ordinary
                // direct `call @name`.
                let fnaddr = self.process_expr(callee)[0].into_pointer_value();

                let fixed_count = fn_type.params.len();
                let mut ir_args: Vec<BasicValueEnum> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(arg);
                    // Only the variadic tail needs default-argument
                    // promotion (the shared C ABI rule; see
                    // `crate::abi::variadic_promotion`).
                    if fn_type.is_variadic && i >= fixed_count {
                        for v in value.iter_mut() {
                            *v = self.promote_variadic_value(*v, &arg_type);
                        }
                    }
                    ir_args.extend(value);
                }

                // sret: allocate the scratch slot and prepend its address
                // -- the exact counterpart of `maybe_sret_arg`.
                let sret_slot = self.needs_sret(&fn_type.return_type).then(|| {
                    let shift = layout::stack_align_shift(layout::type_alignment(&fn_type.return_type));
                    let size = layout::total_bytes(&fn_type.return_type, self.pointer_bytes());
                    self.entry_alloca(size, 1u32 << shift, "sret")
                });
                let mut call_args: Vec<BasicValueEnum> = Vec::new();
                if let Some(slot) = sret_slot {
                    call_args.push(slot.into());
                }
                call_args.extend(ir_args);
                let metadata_args: Vec<inkwell::values::BasicMetadataValueEnum> =
                    call_args.iter().map(|v| (*v).into()).collect();
                let call_type = self.llvm_function_type(fn_type);
                let call = self
                    .builder
                    .build_indirect_call(call_type, fnaddr, &metadata_args, "")
                    .expect("call always succeeds");

                if *fn_type.return_type == ResolvedType::Void {
                    return vec![];
                }
                match sret_slot {
                    Some(slot) => self.load_scalars(
                        &PlaceStorage::Slot { slot, offset: 0 },
                        &fn_type.return_type,
                        layout::type_alignment(&fn_type.return_type),
                    ),
                    None => {
                        let value = match call.try_as_basic_value() {
                            ValueKind::Basic(value) => value,
                            ValueKind::Instruction(_) => {
                                unreachable!("a non-void call always returns a value")
                            }
                        };
                        // A multi-leaf return comes back as one aggregate
                        // struct -- extract it back into leaves.
                        let leaves = leaf::llvm_leaves(self.context, &fn_type.return_type, self.pointer_bytes());
                        if leaves.len() > 1 {
                            (0..leaves.len())
                                .map(|i| {
                                    self.builder
                                        .build_extract_value(value.into_struct_value(), i as u32, "")
                                        .expect("extractvalue on the return aggregate")
                                })
                                .map(|v| v.as_basic_value_enum())
                                .collect()
                        } else {
                            vec![value]
                        }
                    }
                }
            }

            MirExpr::SpecCoerce(MirSpecCoerce { base, slots }) => {
                let data_ptr = self.process_expr(base)[0].into_pointer_value();
                let (spec, type_args) = match &node.r#type {
                    ResolvedType::SpecObject { spec, type_args, .. } => (spec.clone(), type_args.clone()),
                    _ => unreachable!("mir body guarantees a SpecCoerce's own type is SpecObject"),
                };
                let concrete = match &base.r#type {
                    ResolvedType::Pointer { pointee, .. } => (**pointee).clone(),
                    _ => unreachable!("mir body guarantees a SpecCoerce's base is a plain pointer"),
                };
                let vtable = self.vtable_for(&concrete, &spec, &type_args, slots);
                vec![data_ptr.into(), vtable.as_pointer_value().into()]
            }

            MirExpr::DynamicCall(MirDynamicCall { base, slot_index, fn_type, args }) => {
                let base_leaves = self.get_place_value(base);
                let data_ptr = base_leaves[0].into_pointer_value();
                let vtable_ptr = base_leaves[1].into_pointer_value();

                let fnaddr = self.aligned_load_ptr(vtable_ptr, *slot_index as u32 * self.pointer_bytes(), self.pointer_bytes());

                let mut ir_args: Vec<BasicValueEnum> = vec![data_ptr.into()];
                for arg in args {
                    ir_args.extend(self.process_expr(arg));
                }
                let sret_slot = self.needs_sret(&fn_type.return_type).then(|| {
                    let shift = layout::stack_align_shift(layout::type_alignment(&fn_type.return_type));
                    let size = layout::total_bytes(&fn_type.return_type, self.pointer_bytes());
                    self.entry_alloca(size, 1u32 << shift, "sret")
                });
                let mut call_args: Vec<BasicValueEnum> = Vec::new();
                if let Some(slot) = sret_slot {
                    call_args.push(slot.into());
                }
                call_args.extend(ir_args);
                let metadata_args: Vec<inkwell::values::BasicMetadataValueEnum> =
                    call_args.iter().map(|v| (*v).into()).collect();
                let call_type = self.llvm_function_type(fn_type);
                let call = self
                    .builder
                    .build_indirect_call(call_type, fnaddr, &metadata_args, "")
                    .expect("call always succeeds");

                if *fn_type.return_type == ResolvedType::Void {
                    return vec![];
                }
                match sret_slot {
                    Some(slot) => self.load_scalars(
                        &PlaceStorage::Slot { slot, offset: 0 },
                        &fn_type.return_type,
                        layout::type_alignment(&fn_type.return_type),
                    ),
                    None => {
                        let value = match call.try_as_basic_value() {
                            ValueKind::Basic(value) => value,
                            ValueKind::Instruction(_) => {
                                unreachable!("a non-void call always returns a value")
                            }
                        };
                        let leaves = leaf::llvm_leaves(self.context, &fn_type.return_type, self.pointer_bytes());
                        if leaves.len() > 1 {
                            (0..leaves.len())
                                .map(|i| {
                                    self.builder
                                        .build_extract_value(value.into_struct_value(), i as u32, "")
                                        .expect("extractvalue on the return aggregate")
                                })
                                .map(|v| v.as_basic_value_enum())
                                .collect()
                        } else {
                            vec![value]
                        }
                    }
                }
            }

            MirExpr::Number(value) => {
                let raw_leaf = omega_analyzer::layout::leaves_of(&node.r#type, self.pointer_bytes())[0];
                vec![self.scalar_const(raw_leaf, value)]
            }

            MirExpr::Bool(b) => vec![self.context.i8_type().const_int(u64::from(*b), false).into()],

            MirExpr::Sizeof(target_type) => {
                let size = layout::total_bytes(target_type, self.pointer_bytes());
                let int_ty: BasicTypeEnum = if self.pointer_bytes() == 8 {
                    self.context.i64_type().into()
                } else {
                    self.context.i32_type().into()
                };
                vec![int_ty.into_int_type().const_int(size as u64, false).into()]
            }

            MirExpr::Char(c) => vec![self.context.i32_type().const_int(*c as u64, false).into()],

            MirExpr::Place(place) => self.get_place_value(place),

            MirExpr::Assignment(MirAssignment { target, value }) => {
                let values = self.process_expr(value);
                let (storage, _ty, align) = self.resolve_place_storage(target);
                match storage {
                    PlaceStorage::Slot { slot, offset } => {
                        self.store_scalars(&slot, offset, &values, align);
                    }
                    PlaceStorage::Address { base, offset } => {
                        self.store_scalars(&base, offset, &values, align);
                    }
                    PlaceStorage::Values(_) => unreachable!(
                        "assignment into a function parameter is rejected by the shared preflight (crate::preflight) before any backend runs"
                    ),
                }
                values
            }

            MirExpr::AddressOf(MirAddressOf { place }) => {
                let (storage, _ty, _align) = self.resolve_place_storage(place);
                vec![self.place_storage_address(&storage).into()]
            }

            MirExpr::Negate(base) => {
                let is_float = matches!(
                    base.r#type.numeric_kind(self.target.pointer_bits()),
                    Some(NumericKind::Float(_))
                );
                let value = self.process_expr(base)[0];
                let result = if is_float {
                    self.builder
                        .build_float_neg(value.into_float_value(), "neg")
                        .expect("fneg always succeeds")
                        .as_basic_value_enum()
                } else {
                    self.builder
                        .build_int_neg(value.into_int_value(), "neg")
                        .expect("ineg always succeeds")
                        .as_basic_value_enum()
                };
                vec![result]
            }

            MirExpr::BitNot(base) => {
                let value = self.process_expr(base)[0].into_int_value();
                vec![self
                    .builder
                    .build_not(value, "bnot")
                    .expect("bnot always succeeds")
                    .as_basic_value_enum()]
            }

            MirExpr::BinaryOp(MirBinaryOp { op, left, right }) => {
                let kind = match &left.r#type {
                    ResolvedType::Char => NumericKind::Unsigned(32),
                    ResolvedType::Bool => NumericKind::Unsigned(8),
                    r#type => r#type
                        .numeric_kind(self.target.pointer_bits())
                        .expect("mir body guarantees BinaryOp operands are numeric, char, or bool"),
                };
                let left_value = self.process_expr(left)[0];
                let right_value = self.process_expr(right)[0];
                // A `Ptr` leaf reaches every integer-domain op, not just the
                // comparisons. Pointer arithmetic coerces to `usize` at
                // analysis time (`ResolvedType::arithmetic_repr`), but the
                // operand keeps its pointer leaf all the way here -- and a
                // pointer *parameter* arrives with no cast in front of it at
                // all. Cranelift's integer instructions accept such a value
                // natively, since its pointer type is an integer type; LLVM's
                // need a genuine integer, so normalize once here rather than
                // at each of the fifteen arms below.
                let (left, right) = match kind {
                    NumericKind::Float(_) => (left_value, right_value),
                    _ => (
                        self.to_int_operand(left_value).as_basic_value_enum(),
                        self.to_int_operand(right_value).as_basic_value_enum(),
                    ),
                };

                let result = match (op, kind) {
                    (BinaryOp::Add, NumericKind::Float(_)) => self
                        .builder
                        .build_float_add(left.into_float_value(), right.into_float_value(), "add")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Add, _) => self
                        .builder
                        .build_int_add(left.into_int_value(), right.into_int_value(), "add")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Sub, NumericKind::Float(_)) => self
                        .builder
                        .build_float_sub(left.into_float_value(), right.into_float_value(), "sub")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Sub, _) => self
                        .builder
                        .build_int_sub(left.into_int_value(), right.into_int_value(), "sub")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Mul, NumericKind::Float(_)) => self
                        .builder
                        .build_float_mul(left.into_float_value(), right.into_float_value(), "mul")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Mul, _) => self
                        .builder
                        .build_int_mul(left.into_int_value(), right.into_int_value(), "mul")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Div, NumericKind::Float(_)) => self
                        .builder
                        .build_float_div(left.into_float_value(), right.into_float_value(), "div")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Div, NumericKind::Signed(_)) => self
                        .builder
                        .build_int_signed_div(left.into_int_value(), right.into_int_value(), "sdiv")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Div, NumericKind::Unsigned(_)) => self
                        .builder
                        .build_int_unsigned_div(left.into_int_value(), right.into_int_value(), "udiv")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Rem, NumericKind::Signed(_)) => self
                        .builder
                        .build_int_signed_rem(left.into_int_value(), right.into_int_value(), "srem")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Rem, NumericKind::Unsigned(_)) => self
                        .builder
                        .build_int_unsigned_rem(left.into_int_value(), right.into_int_value(), "urem")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Rem, NumericKind::Float(_)) => {
                        unreachable!("mir body rejects '%' on float operands")
                    }
                    (BinaryOp::BitAnd, _) => self
                        .builder
                        .build_and(left.into_int_value(), right.into_int_value(), "and")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::BitOr, _) => self
                        .builder
                        .build_or(left.into_int_value(), right.into_int_value(), "or")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::BitXor, _) => self
                        .builder
                        .build_xor(left.into_int_value(), right.into_int_value(), "xor")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Shl, _) => self
                        .builder
                        .build_left_shift(left.into_int_value(), right.into_int_value(), "shl")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Shr, NumericKind::Signed(_)) => self
                        .builder
                        .build_right_shift(left.into_int_value(), right.into_int_value(), true, "sshr")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Shr, NumericKind::Unsigned(_)) => self
                        .builder
                        .build_right_shift(left.into_int_value(), right.into_int_value(), false, "ushr")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Shr, NumericKind::Float(_)) => {
                        unreachable!("mir body rejects '>>' on float operands")
                    }
                    (cmp, NumericKind::Float(_)) => {
                        use inkwell::FloatPredicate::*;
                        let pred = match cmp {
                            BinaryOp::Eq => OEQ,
                            BinaryOp::Ne => ONE,
                            BinaryOp::Lt => OLT,
                            BinaryOp::Le => OLE,
                            BinaryOp::Gt => OGT,
                            BinaryOp::Ge => OGE,
                            _ => unreachable!("not a comparison op"),
                        };
                        let cmp = self
                            .builder
                            .build_float_compare(pred, left.into_float_value(), right.into_float_value(), "fcmp")
                            .unwrap();
                        self.bool_result(cmp)
                    }
                    (cmp, NumericKind::Signed(_)) => {
                        use inkwell::IntPredicate::*;
                        let pred = match cmp {
                            BinaryOp::Eq => EQ,
                            BinaryOp::Ne => NE,
                            BinaryOp::Lt => SLT,
                            BinaryOp::Le => SLE,
                            BinaryOp::Gt => SGT,
                            BinaryOp::Ge => SGE,
                            _ => unreachable!("not a comparison op"),
                        };
                        // Pointer leaves can reach a comparison directly:
                        // pointer arithmetic *coerces* to `usize` at analysis
                        // time, but the coercion is a `Reinterpret` cast, so
                        // the value keeps its pointer leaf. Cranelift's
                        // `icmp` accepts pointer-typed values natively;
                        // LLVM's wants integers, so translate.
                        let left = self.to_int_operand(left);
                        let right = self.to_int_operand(right);
                        let cmp = self
                            .builder
                            .build_int_compare(pred, left, right, "icmp")
                            .unwrap();
                        self.bool_result(cmp)
                    }
                    (cmp, NumericKind::Unsigned(_)) => {
                        use inkwell::IntPredicate::*;
                        let pred = match cmp {
                            BinaryOp::Eq => EQ,
                            BinaryOp::Ne => NE,
                            BinaryOp::Lt => ULT,
                            BinaryOp::Le => ULE,
                            BinaryOp::Gt => UGT,
                            BinaryOp::Ge => UGE,
                            _ => unreachable!("not a comparison op"),
                        };
                        let left = self.to_int_operand(left);
                        let right = self.to_int_operand(right);
                        let cmp = self
                            .builder
                            .build_int_compare(pred, left, right, "icmp")
                            .unwrap();
                        self.bool_result(cmp)
                    }
                };
                // ...and back out again, when the expression is itself
                // pointer-typed (`ptr + n` is a pointer, not a `usize`) --
                // the same invariant the operand normalization above keeps,
                // in the other direction. A comparison's `i1` result is left
                // alone: neither side of that is a pointer domain.
                let want = leaf::llvm_leaves(self.context, &node.r#type, self.pointer_bytes());
                let result = match want.first() {
                    Some(want) => self.reinterpret_leaf(result, *want),
                    None => result,
                };
                vec![result]
            }

            MirExpr::ArrayLiteral(MirArrayLiteral { elements, .. }) => {
                elements.iter().flat_map(|e| self.process_expr(e)).collect()
            }

            MirExpr::StructLiteral(MirStructLiteral { fields }) => {
                let ResolvedType::Struct(struct_type) = &node.r#type else {
                    unreachable!("mir body guarantees a struct literal's own type is a struct");
                };
                let field_count = struct_type.borrow().fields.len();
                let mut per_field: Vec<Option<Vec<BasicValueEnum>>> = vec![None; field_count];
                for field in fields {
                    per_field[field.field_index] = Some(self.process_expr(&field.value));
                }
                per_field
                    .into_iter()
                    .flat_map(|leaves| leaves.expect("mir body guarantees every field is initialized"))
                    .collect()
            }

            MirExpr::EnumConstruct(MirEnumConstruct { variant_index, fields }) => {
                let ResolvedType::Enum { cell, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a construction's own type is its enum");
                };
                let cell = cell.clone();
                let pointer_bytes = self.pointer_bytes();
                let (tag, tag_type, header, payload_offset, chunk_leaves, field_offsets) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[*variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|((_, r#type, _), value)| (r#type.clone(), value.clone()))
                        .collect();
                    let field_offsets: Vec<u32> = (0..enum_type.dynamic_fields.len())
                        .map(|i| layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes))
                        .chain(
                            (0..variant.fields.len())
                                .map(|i| layout::enum_body_field_offset(&enum_type, *variant_index, i, pointer_bytes)),
                        )
                        .collect();
                    let payload_offset = layout::enum_payload_offset(&enum_type, pointer_bytes);
                    let chunk_leaves = layout::payload_chunks(layout::enum_payload_bytes(
                        &enum_type,
                        enum_type.layout.pack,
                        pointer_bytes,
                    ));
                    (variant.tag, enum_type.tag_type.clone(), header, payload_offset, chunk_leaves, field_offsets)
                };

                let shift = layout::stack_align_shift(layout::type_alignment(&node.r#type));
                let total = layout::total_bytes(&node.r#type, pointer_bytes);
                let slot = self.entry_alloca(total, 1u32 << shift, "enum");

                let tag_values = self.emit_const_value(&ConstValue::Number(tag), &tag_type);
                self.store_scalars(&slot, 0, &tag_values, layout::type_alignment(&tag_type));

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (r#type, value) in &header {
                    let const_values = self.emit_const_value(value, r#type);
                    self.store_scalars(&slot, offset, &const_values, layout::type_alignment(r#type));
                    offset += layout::total_bytes(r#type, pointer_bytes);
                }

                let mut chunk_offset = payload_offset;
                for raw_leaf in &chunk_leaves {
                    let llvm_ty = leaf::llvm_type(self.context, *raw_leaf, self.pointer_bytes());
                    let zero = match llvm_ty {
                        BasicTypeEnum::IntType(it) => it.const_zero().into(),
                        BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
                        BasicTypeEnum::PointerType(_) => self.ptr_type().const_null().into(),
                        _ => unreachable!("a payload chunk is always a scalar"),
                    };
                    self.store_scalars(&slot, chunk_offset, &[zero], 1);
                    chunk_offset += raw_leaf.bytes(pointer_bytes);
                }

                for field in fields {
                    let field_offset = field_offsets[field.field_index];
                    let values = self.process_expr(&field.value);
                    self.store_scalars(&slot, field_offset, &values, 1);
                }

                self.load_scalars(
                    &PlaceStorage::Slot { slot, offset: 0 },
                    &node.r#type,
                    layout::type_alignment(&node.r#type),
                )
            }

            MirExpr::UnionConstruct(MirUnionConstruct { field_index: _, value }) => {
                let total = layout::total_bytes(&node.r#type, self.pointer_bytes());
                let slot = self.entry_alloca(total, 16, "union");

                let mut chunk_offset = 0u32;
                for raw_leaf in omega_analyzer::layout::leaves_of(&node.r#type, self.pointer_bytes()) {
                    let llvm_ty = leaf::llvm_type(self.context, raw_leaf, self.pointer_bytes());
                    let zero = match llvm_ty {
                        BasicTypeEnum::IntType(it) => it.const_zero().into(),
                        BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
                        BasicTypeEnum::PointerType(_) => self.ptr_type().const_null().into(),
                        _ => unreachable!("a union chunk is always a scalar"),
                    };
                    self.store_scalars(&slot, chunk_offset, &[zero], 1);
                    chunk_offset += raw_leaf.bytes(self.pointer_bytes());
                }

                let values = self.process_expr(value);
                self.store_scalars(&slot, 0, &values, 1);

                self.load_scalars(
                    &PlaceStorage::Slot { slot, offset: 0 },
                    &node.r#type,
                    layout::type_alignment(&node.r#type),
                )
            }

            MirExpr::Slice(MirSlice { base, item_type, start, end, inclusive }) => {
                let (storage, base_type, _align) = self.resolve_place_storage(base);

                let (data_ptr, full_len) = match &base_type {
                    ResolvedType::SizedArray(_, size) => {
                        let ptr = self.place_storage_address(&storage);
                        let len: BasicValueEnum = self.context.i32_type().const_int(*size as u64, false).into();
                        (ptr, len)
                    }
                    ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
                        let leaves = self.load_scalars(&storage, &base_type, layout::type_alignment(&base_type));
                        (leaves[0].into_pointer_value(), leaves[1])
                    }
                    ResolvedType::Array(_, _) => {
                        let leaves = self.load_scalars(&storage, &base_type, layout::type_alignment(&base_type));
                        (leaves[0].into_pointer_value(), self.context.i32_type().const_int(0, false).into())
                    }
                    _ => unreachable!("mir body guarantees a slice's base is SizedArray/Slice/Str/Array"),
                };

                let elem_size = layout::total_bytes(item_type, self.pointer_bytes()) as i64;
                let start_val = match start {
                    Some(e) => self.process_expr(e)[0],
                    None => self.context.i32_type().const_int(0, false).into(),
                };
                let end_val = match end {
                    Some(e) => {
                        let v = self.process_expr(e)[0].into_int_value();
                        if *inclusive {
                            self.builder
                                .build_int_add(v, self.context.i32_type().const_int(1, false), "inc")
                                .unwrap()
                                .as_basic_value_enum()
                        } else {
                            v.into()
                        }
                    }
                    None => full_len,
                };

                let ptr_int: BasicTypeEnum = if self.pointer_bytes() == 8 {
                    self.context.i64_type().into()
                } else {
                    self.context.i32_type().into()
                };
                let start_ext = self
                    .builder
                    .build_int_z_extend(start_val.into_int_value(), ptr_int.into_int_type(), "start")
                    .unwrap();
                let byte_offset = self
                    .builder
                    .build_int_mul(start_ext, ptr_int.into_int_type().const_int(elem_size as u64, true), "byteoff")
                    .unwrap();
                let new_ptr = self.gep(data_ptr, byte_offset);
                let new_len = self
                    .builder
                    .build_int_sub(end_val.into_int_value(), start_val.into_int_value(), "slicelen")
                    .unwrap();

                vec![new_ptr.into(), new_len.into()]
            }

            MirExpr::Cast(MirCast { kind, target_type, base }) => {
                let unsize_len = match &base.r#type {
                    ResolvedType::Pointer { pointee, .. } => match pointee.as_ref() {
                        ResolvedType::SizedArray(_, size) => Some(*size),
                        _ => None,
                    },
                    _ => None,
                };
                let base_leaves = self.process_expr(base);
                let target_ir = leaf::llvm_leaves(self.context, target_type, self.pointer_bytes())[0];
                match kind {
                    CastKind::Reinterpret => {
                        let target_leaves =
                            leaf::llvm_leaves(self.context, target_type, self.pointer_bytes());
                        base_leaves
                            .into_iter()
                            .zip(target_leaves)
                            .map(|(value, want)| self.reinterpret_leaf(value, want))
                            .collect()
                    }
                    CastKind::DropLength => vec![base_leaves[0]],
                    CastKind::Unsize => {
                        let len = unsize_len.expect("mir body guarantees Unsize's base is Pointer{SizedArray}");
                        vec![
                            base_leaves[0],
                            self.context.i32_type().const_int(len as u64, false).into(),
                        ]
                    }
                    // A pointer source (`<usize>ptr`, `<i64>ptr` -- pointers
                    // classify as pointer-width unsigned integers for casts)
                    // keeps its pointer *leaf*; translate it to an integer
                    // before the integer-shaped conversion, exactly like the
                    // comparison arms above.
                    CastKind::IntExtend { signed } => {
                        let base_int = self.to_int_operand(base_leaves[0]);
                        vec![self.cast_to_target_leaf(base_int, target_ir, *signed)]
                    }
                    CastKind::IntTruncate => {
                        let base_int = self.to_int_operand(base_leaves[0]);
                        vec![self.cast_to_target_leaf(base_int, target_ir, false)]
                    }
                    CastKind::IntToFloat { signed: true } => {
                        let base_int = self.to_int_operand(base_leaves[0]);
                        vec![self
                            .builder
                            .build_signed_int_to_float(base_int, target_ir.into_float_type(), "sitofp")
                            .unwrap()
                            .as_basic_value_enum()]
                    }
                    CastKind::IntToFloat { signed: false } => {
                        let base_int = self.to_int_operand(base_leaves[0]);
                        vec![self
                            .builder
                            .build_unsigned_int_to_float(base_int, target_ir.into_float_type(), "uitofp")
                            .unwrap()
                            .as_basic_value_enum()]
                    }
                    // Saturating, matching Cranelift's `fcvt_to_*_sat` --
                    // the language's own FloatToInt semantics. The
                    // intrinsic is *overloaded* on both its result and its
                    // argument type, so it is looked up by base name and
                    // declared through LLVM's own mangler rather than by
                    // spelling `llvm.fptosi.sat.<to>.<from>` out here --
                    // the type suffixes LLVM wants (`f32`, not a printed
                    // `float`) are its own business, and a hand-built name
                    // that misses simply finds no declaration at all.
                    // A pointer target (`<*u8>some_float`: a pointer
                    // classifies as a pointer-width unsigned integer for
                    // casts) converts to the pointer-width integer first,
                    // then `inttoptr`, exactly like `cast_to_target_leaf`.
                    CastKind::FloatToInt { signed } => {
                        let from = base_leaves[0].into_float_value().get_type();
                        let to = if target_ir.is_pointer_type() {
                            leaf::size_type(self.context, self.pointer_bytes())
                        } else {
                            target_ir.into_int_type()
                        };
                        let family = if *signed { "llvm.fptosi.sat" } else { "llvm.fptoui.sat" };
                        let intrinsic = inkwell::intrinsics::Intrinsic::find(family)
                            .and_then(|intrinsic| {
                                intrinsic.get_declaration(&self.module, &[to.into(), from.into()])
                            })
                            .unwrap_or_else(|| {
                                panic!("saturating float-to-int intrinsic '{family}' unavailable")
                            });
                        let intrinsic_args: [inkwell::values::BasicMetadataValueEnum; 1] =
                            [base_leaves[0].as_basic_value_enum().into()];
                        let value = match self
                            .builder
                            .build_call(intrinsic, &intrinsic_args, "fptoint.sat")
                            .unwrap()
                            .try_as_basic_value()
                        {
                            ValueKind::Basic(value) => value,
                            ValueKind::Instruction(_) => {
                                unreachable!("the sat intrinsic returns a value")
                            }
                        };
                        vec![self.reinterpret_leaf(value, target_ir)]
                    }
                    CastKind::FloatExtend => vec![self
                        .builder
                        .build_float_ext(base_leaves[0].into_float_value(), target_ir.into_float_type(), "fpext")
                        .unwrap()
                        .as_basic_value_enum()],
                    CastKind::FloatTruncate => vec![self
                        .builder
                        .build_float_trunc(base_leaves[0].into_float_value(), target_ir.into_float_type(), "fptrunc")
                        .unwrap()
                        .as_basic_value_enum()],
                    CastKind::SpecNarrow { slot_offset } => {
                        let byte_offset = *slot_offset as i64 * self.pointer_bytes() as i64;
                        let ptr_int: BasicTypeEnum = if self.pointer_bytes() == 8 {
                            self.context.i64_type().into()
                        } else {
                            self.context.i32_type().into()
                        };
                        let vtable = self.gep(
                            base_leaves[1].into_pointer_value(),
                            ptr_int.into_int_type().const_int(byte_offset as u64, true),
                        );
                        vec![base_leaves[0], vtable.into()]
                    }
                }
            }
        }
    }

    /// One scalar constant of `raw_leaf`'s own type -- `Ptr` (a `usize`
    /// literal's leaf) is an integer constant of pointer width cast to the
    /// opaque pointer, mirroring Cranelift's `iconst` of a pointer type.
    fn scalar_const(
        &self,
        raw_leaf: omega_analyzer::layout::Leaf,
        value: &NumberValue,
    ) -> BasicValueEnum<'ctx> {
        use omega_analyzer::layout::Leaf;
        let int_of_width = |width: u32| -> inkwell::types::IntType {
            match width {
                8 => self.context.i8_type(),
                16 => self.context.i16_type(),
                32 => self.context.i32_type(),
                _ => self.context.i64_type(),
            }
        };
        match raw_leaf {
            Leaf::Ptr => {
                let width = self.pointer_bytes() * 8;
                let int_ty = int_of_width(width);
                let int_value = match value {
                    NumberValue::Signed(v) => int_ty.const_int(*v as u64, true),
                    NumberValue::Unsigned(v) => int_ty.const_int(*v, false),
                    NumberValue::Float(_) => unreachable!("a pointer literal is never a float"),
                };
                int_value.const_to_pointer(self.ptr_type()).into()
            }
            // Pointer-*width integer* (`usize`/`isize`): the same bits as
            // the `Ptr` arm above, minus the `const_to_pointer` -- this one
            // stays an integer, which is the whole reason `Leaf` separates
            // the two (see `omega_analyzer::layout::Leaf`).
            Leaf::Size => {
                let int_ty = leaf::size_type(self.context, self.pointer_bytes());
                match value {
                    NumberValue::Signed(v) => int_ty.const_int(*v as u64, true).into(),
                    NumberValue::Unsigned(v) => int_ty.const_int(*v, false).into(),
                    NumberValue::Float(_) => unreachable!("a usize/isize literal is never a float"),
                }
            }
            Leaf::I8 | Leaf::I16 | Leaf::I32 | Leaf::I64 => {
                let width = match raw_leaf {
                    Leaf::I8 => 8,
                    Leaf::I16 => 16,
                    Leaf::I32 => 32,
                    _ => 64,
                };
                let int_ty = int_of_width(width);
                match value {
                    NumberValue::Signed(v) => int_ty.const_int(*v as u64, true).into(),
                    NumberValue::Unsigned(v) => int_ty.const_int(*v, false).into(),
                    NumberValue::Float(_) => unreachable!("an integer literal is never a float"),
                }
            }
            Leaf::F32 | Leaf::F64 => {
                let float_ty = if raw_leaf == Leaf::F32 { self.context.f32_type() } else { self.context.f64_type() };
                match value {
                    NumberValue::Float(v) => float_ty.const_float(*v).into(),
                    _ => unreachable!("a float literal is always a NumberValue::Float"),
                }
            }
        }
    }

    /// An integer-shaped cast's target-side translation: extend/truncate to
    /// the target's width, then `inttoptr` when the target leaf is itself a
    /// pointer (`<*u8>some_int` -- pointers classify as pointer-width
    /// integers for casts, and Cranelift's unified type system folds the
    /// two into one instruction; LLVM's keeps them apart).
    fn cast_to_target_leaf(
        &self,
        base: inkwell::values::IntValue<'ctx>,
        target: BasicTypeEnum<'ctx>,
        signed: bool,
    ) -> BasicValueEnum<'ctx> {
        let target_width = if target.is_pointer_type() {
            self.pointer_bytes() * 8
        } else {
            target.into_int_type().get_bit_width()
        };
        let int_ty = match target_width {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            _ => self.context.i64_type(),
        };
        let source_width = base.get_type().get_bit_width();
        let value = if source_width < target_width {
            if signed {
                self.builder
                    .build_int_s_extend(base, int_ty, "sext")
                    .unwrap()
                    .as_basic_value_enum()
            } else {
                self.builder
                    .build_int_z_extend(base, int_ty, "zext")
                    .unwrap()
                    .as_basic_value_enum()
            }
        } else if source_width > target_width {
            self.builder
                .build_int_truncate(base, int_ty, "trunc")
                .unwrap()
                .as_basic_value_enum()
        } else {
            base.into()
        };
        if target.is_pointer_type() {
            self.builder
                .build_int_to_ptr(value.into_int_value(), self.ptr_type(), "inttoptr")
                .expect("inttoptr always succeeds")
                .as_basic_value_enum()
        } else {
            value
        }
    }

    /// One leaf, retyped for a `Reinterpret` cast.
    ///
    /// A `Reinterpret` is a pure relabeling in the MIR, and a genuine no-op
    /// in Cranelift, where an address simply *is* a pointer-width integer.
    /// LLVM keeps `ptr` and `iN` distinct, so the same relabeling needs a
    /// real `ptrtoint`/`inttoptr` whenever it crosses that boundary --
    /// which pointer arithmetic does on every use, since the analyzer
    /// coerces a pointer operand to `usize` through exactly this cast (see
    /// `ResolvedType::arithmetic_repr`).
    ///
    /// This is what keeps the backend's central invariant true: a value's
    /// LLVM type always matches the leaf list of its MIR type. Without it
    /// a pointer keeps its `ptr` leaf while claiming to be a `usize`, and
    /// every downstream integer consumer -- arithmetic, bitwise ops,
    /// comparisons -- receives a value of the wrong LLVM type.
    ///
    /// Same-domain reinterprets stay no-ops: `*T` to `*U` is nothing at all
    /// under opaque pointers, and an integer relabeled to a same-width
    /// integer needs no instruction either.
    fn reinterpret_leaf(
        &self,
        value: BasicValueEnum<'ctx>,
        want: inkwell::types::BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match (value.is_pointer_value(), want.is_pointer_type()) {
            (true, false) => self.to_int_operand(value).as_basic_value_enum(),
            (false, true) => self
                .builder
                .build_int_to_ptr(value.into_int_value(), self.ptr_type(), "inttoptr")
                .expect("inttoptr always succeeds")
                .as_basic_value_enum(),
            _ => value,
        }
    }

    /// An `icmp`/`fcmp` result (`i1`), widened to the `i8` that Omega's
    /// `bool` actually is.
    ///
    /// The invariant this half maintains: **a `bool`-typed value is always
    /// `i8` in this backend**, because that is what `layout::leaves_of`
    /// says it is (`ResolvedType::Bool => vec![Leaf::I8]`), and every other
    /// producer of a `bool` -- a load, a call result, a struct field --
    /// already yields one. LLVM's comparisons are the sole exception, so
    /// they are converted at the source rather than every consumer being
    /// taught to accept both widths. `to_i1` below is the other half: the
    /// single point where an `i8` becomes `i1` again, because `br` is the
    /// only instruction that demands it.
    ///
    /// Cranelift needs neither: its `icmp` yields an `I8` directly and its
    /// `brif` accepts it, so the two widths never diverge there.
    fn bool_result(&self, value: inkwell::values::IntValue<'ctx>) -> BasicValueEnum<'ctx> {
        self.builder
            .build_int_z_extend(value, self.context.i8_type(), "tobool8")
            .expect("zext always succeeds")
            .as_basic_value_enum()
    }

    /// `value`, narrowed to the `i1` that LLVM's `br` requires -- the
    /// counterpart of `bool_result` above, and the only place an Omega
    /// `bool` stops being an `i8`. Already-`i1` values pass through so the
    /// helper is safe to apply unconditionally.
    pub(super) fn to_i1(&self, value: inkwell::values::IntValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        if value.get_type().get_bit_width() == 1 {
            return value;
        }
        self.builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                value,
                value.get_type().const_zero(),
                "tobool1",
            )
            .expect("icmp always succeeds")
    }

    /// `v`, translated to an integer operand where a comparison needs one
    /// -- a pointer value (see the `BinaryOp` arm) becomes
    /// `ptrtoint`-of-pointer-width; an integer passes through unchanged.
    fn to_int_operand(&self, v: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        if v.is_pointer_value() {
            let int_ty = leaf::size_type(self.context, self.pointer_bytes());
            self.builder
                .build_ptr_to_int(v.into_pointer_value(), int_ty, "ptrtoint")
                .expect("ptrtoint always succeeds")
        } else {
            v.into_int_value()
        }
    }

    /// The C default-argument-promotion emission for one variadic argument
    /// -- the *decision* is the shared rule in
    /// `crate::abi::variadic_promotion`; this only emits the conversion.
    fn promote_variadic_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        arg_type: &ResolvedType,
    ) -> BasicValueEnum<'ctx> {
        match crate::abi::variadic_promotion(arg_type, self.target) {
            Some(NumericKind::Float(_)) => self
                .builder
                .build_float_ext(value.into_float_value(), self.context.f64_type(), "fpromote")
                .unwrap()
                .as_basic_value_enum(),
            Some(NumericKind::Signed(_)) => self
                .builder
                .build_int_s_extend(value.into_int_value(), self.context.i32_type(), "spromote")
                .unwrap()
                .as_basic_value_enum(),
            Some(NumericKind::Unsigned(_)) => self
                .builder
                .build_int_z_extend(value.into_int_value(), self.context.i32_type(), "upromote")
                .unwrap()
                .as_basic_value_enum(),
            None => value,
        }
    }

    /// Reads a whole place's leaves back -- `cranelift::get_place_value`'s
    /// LLVM counterpart (function roots yield their own address).
    pub(super) fn get_place_value(&mut self, place: &omega_mir::MirPlace) -> Vec<BasicValueEnum<'ctx>> {
        // A function reference has no memory backing at all -- just a
        // symbol address (the exact counterpart of cranelift's own
        // Function-root handling in `get_place_value`).
        if let omega_mir::MirPlaceRoot::Function(decl_id) = &place.root {
            let function = *self
                .functions
                .get(decl_id)
                .expect("every function is declared before any body references it");
            return vec![function.as_global_value().as_pointer_value().into()];
        }
        let (storage, r#type, align) = self.resolve_place_storage(place);
        match &storage {
            PlaceStorage::Values(values) => values.clone(),
            PlaceStorage::Slot { .. } | PlaceStorage::Address { .. } => {
                self.load_scalars(&storage, &r#type, align)
            }
        }
    }

    /// A byte-run constant's two-leaf `[pointer, length]` form -- the LLVM
    /// counterpart of `cranelift::emit_bytes` (same dedup map, same
    /// shared content-hashed symbol).
    fn emit_bytes(&mut self, s: String) -> Vec<BasicValueEnum<'ctx>> {
        let len = self.context.i32_type().const_int(s.len() as u64, false);
        let data = self.get_or_declare_global_bytes(s);
        vec![data.as_pointer_value().into(), len.into()]
    }

    /// Declares (and defines) `s`'s bytes as an anonymous module-level data
    /// object, verbatim -- no null terminator, shared by `"..."` and
    /// `b"..."` literals alike (see `cranelift::get_or_declare_global_bytes`).
    fn get_or_declare_global_bytes(&mut self, s: String) -> GlobalValue<'ctx> {
        if let Some(global) = self.bytes.get(&s) {
            return *global;
        }
        let symbol = omega_mir::mangle::data_symbol(s.as_bytes());
        let bytes = s.clone().into_bytes();
        let array_ty = self.context.i8_type().array_type(bytes.len() as u32);
        let global = self.module.add_global(array_ty, None, &symbol);
        global.set_linkage(inkwell::module::Linkage::WeakODR);
        global.set_constant(true);
        let elems: Vec<inkwell::values::IntValue> = bytes
            .iter()
            .map(|b| self.context.i8_type().const_int(*b as u64, false))
            .collect();
        global.set_initializer(&self.context.i8_type().const_array(&elems));
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".rodata.{symbol}")));
        }
        self.bytes.insert(s, global);
        global
    }

    /// Builds one anonymous data object holding `elements` at consecutive
    /// `total_bytes(item_type)`-sized slots -- the LLVM counterpart of
    /// `cranelift::build_const_slice_data`.
    fn build_const_slice_data(
        &mut self,
        elements: &[ConstValue],
        item_type: &ResolvedType,
    ) -> GlobalValue<'ctx> {
        let mut hash_input = Vec::new();
        for element in elements {
            self.hash_const_element(&mut hash_input, element, item_type);
        }
        let symbol = omega_mir::mangle::data_symbol(&hash_input);
        if let Some(global) = self.const_blobs.get(&symbol) {
            return *global;
        }

        let stride = layout::total_bytes(item_type, self.pointer_bytes());
        let mut blob = ConstBlob {
            bytes: vec![0u8; stride as usize * elements.len()],
            relocs: Vec::new(),
            pointer_bytes: self.pointer_bytes(),
        };
        for (i, element) in elements.iter().enumerate() {
            self.write_const_element(&mut blob, i as u32 * stride, element, item_type);
        }
        let global = self.declare_blob(&symbol, &blob);
        self.const_blobs.insert(symbol, global);
        global
    }

    /// `build_const_slice_data`'s `ConstValue::Ref` counterpart -- one
    /// `comp`-evaluated value as its own separately addressable static
    /// data object.
    fn build_const_data(&mut self, value: &ConstValue, r#type: &ResolvedType) -> GlobalValue<'ctx> {
        let mut hash_input = Vec::new();
        self.hash_const_element(&mut hash_input, value, r#type);
        let symbol = omega_mir::mangle::data_symbol(&hash_input);
        if let Some(global) = self.const_blobs.get(&symbol) {
            return *global;
        }

        let total = layout::total_bytes(r#type, self.pointer_bytes());
        let mut blob = ConstBlob {
            bytes: vec![0u8; total as usize],
            relocs: Vec::new(),
            pointer_bytes: self.pointer_bytes(),
        };
        self.write_const_element(&mut blob, 0, value, r#type);
        let global = self.declare_blob(&symbol, &blob);
        self.const_blobs.insert(symbol, global);
        global
    }

    /// The whole-`ConstValue` byte-image builder -- used by both the
    /// item-level global initializers and the anonymous blobs above. See
    /// `item.rs`'s `Declaration` arm.
    pub(super) fn build_const_blob(&mut self, value: &ConstValue, r#type: &ResolvedType) -> ConstBlob<'ctx> {
        let total = layout::total_bytes(r#type, self.pointer_bytes());
        let mut blob = ConstBlob {
            bytes: vec![0u8; total as usize],
            relocs: Vec::new(),
            pointer_bytes: self.pointer_bytes(),
        };
        self.write_const_element(&mut blob, 0, value, r#type);
        blob
    }

    /// Declares a `ConstBlob` as a module-level weak data object and
    /// returns its initializer's `(type, value)` pair, so the caller can
    /// `add_global` with exactly the matching type -- a plain byte array
    /// when nothing inside is a pointer, or a *packed* struct of byte
    /// spans and pointer fields when it embeds relocations (LLVM builds
    /// the real relocations from the pointer fields itself).
    pub(super) fn materialize_blob(
        &mut self,
        blob: &ConstBlob<'ctx>,
    ) -> (BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>) {
        let total = blob.bytes.len() as u32;
        if blob.relocs.is_empty() {
            // Exactly `total`, never `total.max(1)`: the initializer below
            // has `total` elements, and a global whose declared type and
            // initializer type disagree is invalid IR.
            let ty: BasicTypeEnum = self.context.i8_type().array_type(total).into();
            let bytes: Vec<inkwell::values::IntValue> = blob
                .bytes
                .iter()
                .map(|b| self.context.i8_type().const_int(*b as u64, false))
                .collect();
            return (ty, self.context.i8_type().const_array(&bytes).into());
        }

        let mut sorted = blob.relocs.clone();
        sorted.sort_by_key(|(offset, _)| *offset);
        let mut fields: Vec<BasicTypeEnum> = Vec::new();
        let mut values: Vec<BasicValueEnum> = Vec::new();
        let mut cursor = 0u32;
        for (offset, target) in &sorted {
            if *offset > cursor {
                let span: Vec<inkwell::values::IntValue> = blob.bytes[cursor as usize..*offset as usize]
                    .iter()
                    .map(|b| self.context.i8_type().const_int(*b as u64, false))
                    .collect();
                fields.push(self.context.i8_type().array_type(span.len() as u32).into());
                values.push(self.context.i8_type().const_array(&span).into());
            }
            fields.push(self.ptr_type().into());
            values.push(target.as_pointer_value().into());
            cursor = offset + blob.pointer_bytes;
        }
        if cursor < total {
            let span: Vec<inkwell::values::IntValue> = blob.bytes[cursor as usize..]
                .iter()
                .map(|b| self.context.i8_type().const_int(*b as u64, false))
                .collect();
            fields.push(self.context.i8_type().array_type(span.len() as u32).into());
            values.push(self.context.i8_type().const_array(&span).into());
        }
        let struct_ty = self.context.struct_type(&fields, true);
        (struct_ty.into(), struct_ty.const_named_struct(&values).into())
    }

    /// `materialize_blob` plus `add_global` under the blob's own symbol --
    /// the anonymous-blob declaration shape (weak, read-only).
    fn declare_blob(&mut self, symbol: &str, blob: &ConstBlob<'ctx>) -> GlobalValue<'ctx> {
        let (ty, init) = self.materialize_blob(blob);
        let global = self.module.add_global(ty, None, symbol);
        global.set_linkage(inkwell::module::Linkage::WeakODR);
        global.set_constant(true);
        global.set_initializer(&init);
        global.set_alignment(1);
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".rodata.{symbol}")));
        }
        global
    }

    /// Emits one `ConstValue` (an enum tag/header constant, or a
    /// `MirExpr::Const`) as its leaves, in leaf order -- the LLVM
    /// counterpart of `cranelift::emit_const_value`.
    fn emit_const_value(
        &mut self,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) -> Vec<BasicValueEnum<'ctx>> {
        match value {
            ConstValue::Number(number) => {
                let raw_leaf = omega_analyzer::layout::leaves_of(r#type, self.pointer_bytes())[0];
                vec![self.scalar_const(raw_leaf, number)]
            }
            ConstValue::Bool(b) => vec![self.context.i8_type().const_int(u64::from(*b), false).into()],
            ConstValue::Char(c) => vec![self.context.i32_type().const_int(*c as u64, false).into()],
            ConstValue::Str(s) => self.emit_bytes(s.clone()),
            ConstValue::Slice(elements) => {
                let item = match r#type {
                    ResolvedType::Slice { item, .. } => item,
                    _ => unreachable!("mir body guarantees a Slice constant's own type is Slice"),
                };
                let len = self.context.i32_type().const_int(elements.len() as u64, false);
                let data = self.build_const_slice_data(elements, item);
                vec![data.as_pointer_value().into(), len.into()]
            }
            ConstValue::Array(elements) => {
                let item = match r#type {
                    ResolvedType::SizedArray(item, _) => item,
                    _ => unreachable!("mir body guarantees an Array constant's own type is SizedArray"),
                };
                elements.iter().flat_map(|element| self.emit_const_value(element, item)).collect()
            }
            ConstValue::Struct(fields) => {
                let struct_type = match r#type {
                    ResolvedType::Struct(struct_type) => struct_type,
                    _ => unreachable!("a Struct constant's own type is always ResolvedType::Struct"),
                };
                let field_types: Vec<ResolvedType> =
                    struct_type.borrow().fields.iter().map(|(_, t, _)| t.clone()).collect();
                fields
                    .iter()
                    .zip(&field_types)
                    .flat_map(|(value, field_type)| self.emit_const_value(value, field_type))
                    .collect()
            }
            ConstValue::Enum { variant_index, tag, dynamic_fields, fields, .. } => {
                let cell = match r#type {
                    ResolvedType::Enum { cell, .. } => cell,
                    _ => unreachable!("an Enum constant's own type is always ResolvedType::Enum"),
                };
                let pointer_bytes = self.pointer_bytes();
                let (tag_type, header, dynamic, body) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[*variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|((_, t, _), v)| (t.clone(), v.clone()))
                        .collect();
                    let dynamic: Vec<(u32, ResolvedType)> = (0..enum_type.dynamic_fields.len())
                        .map(|i| {
                            let offset = layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes);
                            (offset, enum_type.dynamic_fields[i].1.clone())
                        })
                        .collect();
                    let body: Vec<(u32, ResolvedType)> = (0..variant.fields.len())
                        .map(|i| {
                            let offset = layout::enum_body_field_offset(&enum_type, *variant_index, i, pointer_bytes);
                            (offset, variant.fields[i].1.clone())
                        })
                        .collect();
                    (enum_type.tag_type.clone(), header, dynamic, body)
                };

                let shift = layout::stack_align_shift(layout::type_alignment(r#type));
                let total = layout::total_bytes(r#type, pointer_bytes);
                let slot = self.entry_alloca(total, 1u32 << shift, "enumconst");

                let tag_values = self.emit_const_value(&ConstValue::Number(*tag), &tag_type);
                self.store_scalars(&slot, 0, &tag_values, layout::type_alignment(&tag_type));

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    let values = self.emit_const_value(value, field_type);
                    self.store_scalars(&slot, offset, &values, layout::type_alignment(field_type));
                    offset += layout::total_bytes(field_type, pointer_bytes);
                }

                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    let values = self.emit_const_value(value, field_type);
                    self.store_scalars(&slot, *field_offset, &values, layout::type_alignment(field_type));
                }

                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    let values = self.emit_const_value(value, field_type);
                    self.store_scalars(&slot, *field_offset, &values, layout::type_alignment(field_type));
                }

                self.load_scalars(
                    &PlaceStorage::Slot { slot, offset: 0 },
                    r#type,
                    layout::type_alignment(r#type),
                )
            }
            ConstValue::Union { value, .. } => {
                let total = layout::total_bytes(r#type, self.pointer_bytes());
                let slot = self.entry_alloca(total, 16, "unionconst");

                let mut chunk_offset = 0u32;
                for raw_leaf in omega_analyzer::layout::leaves_of(r#type, self.pointer_bytes()) {
                    let llvm_ty = leaf::llvm_type(self.context, raw_leaf, self.pointer_bytes());
                    let zero = match llvm_ty {
                        BasicTypeEnum::IntType(it) => it.const_zero().into(),
                        BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
                        BasicTypeEnum::PointerType(_) => self.ptr_type().const_null().into(),
                        _ => unreachable!("a union chunk is always a scalar"),
                    };
                    self.store_scalars(&slot, chunk_offset, &[zero], 1);
                    chunk_offset += raw_leaf.bytes(self.pointer_bytes());
                }

                let union_type = match r#type {
                    ResolvedType::Union(union_type) => union_type,
                    _ => unreachable!("a Union constant's own type is always ResolvedType::Union"),
                };
                let field_type = union_type.borrow().fields[0].1.clone();
                let values = self.emit_const_value(value, &field_type);
                self.store_scalars(&slot, 0, &values, layout::type_alignment(&field_type));

                self.load_scalars(
                    &PlaceStorage::Slot { slot, offset: 0 },
                    r#type,
                    layout::type_alignment(r#type),
                )
            }
            ConstValue::Ref(inner) => {
                let pointee = match r#type {
                    ResolvedType::Pointer { pointee, .. } => pointee,
                    _ => unreachable!("a Ref constant's own type is always ResolvedType::Pointer"),
                };
                let inner_type = ref_pointee_type(inner, pointee);
                let data = self.build_const_data(inner, &inner_type);
                vec![data.as_pointer_value().into()]
            }
        }
    }

    /// Writes one element's leaves into `blob`'s byte buffer at `offset` --
    /// the LLVM counterpart of `cranelift::write_const_element`, byte-for-
    /// byte identical in what it produces (same layout offsets, same
    /// little-endian writes); pointer-shaped elements record a relocation
    /// instead of bytes, which `materialize_blob` turns into a real LLVM
    /// relocation.
    fn write_const_element(
        &mut self,
        blob: &mut ConstBlob<'ctx>,
        offset: u32,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) {
        let pointer_bytes = self.pointer_bytes();
        match value {
            ConstValue::Number(number) => {
                let leaf = omega_analyzer::layout::leaves_of(r#type, pointer_bytes)[0];
                let leaf_bytes = leaf.bytes(pointer_bytes);
                let raw: u64 = match number {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) if leaf_bytes == 4 => (*v as f32).to_bits() as u64,
                    NumberValue::Float(v) => v.to_bits(),
                };
                let start = offset as usize;
                blob.bytes[start..start + leaf_bytes as usize]
                    .copy_from_slice(&raw.to_le_bytes()[..leaf_bytes as usize]);
            }
            ConstValue::Bool(b) => blob.bytes[offset as usize] = *b as u8,
            ConstValue::Char(c) => {
                let start = offset as usize;
                blob.bytes[start..start + 4].copy_from_slice(&(*c as u32).to_le_bytes());
            }
            ConstValue::Str(s) => {
                let data = if let Some(global) = self.bytes.get(s) {
                    *global
                } else {
                    self.get_or_declare_global_bytes(s.clone())
                };
                blob.relocs.push((offset, data));
                let len_start = (offset + pointer_bytes) as usize;
                blob.bytes[len_start..len_start + 4].copy_from_slice(&(s.len() as i32).to_le_bytes());
            }
            ConstValue::Slice(nested) => {
                let item = match r#type {
                    ResolvedType::Slice { item, .. } => item,
                    _ => unreachable!("mir body guarantees a nested Slice constant's own type is Slice"),
                };
                let nested_id = self.build_const_slice_data(nested, item);
                blob.relocs.push((offset, nested_id));
                let len_start = (offset + pointer_bytes) as usize;
                blob.bytes[len_start..len_start + 4]
                    .copy_from_slice(&(nested.len() as i32).to_le_bytes());
            }
            ConstValue::Array(elements) => {
                let item = match r#type {
                    ResolvedType::SizedArray(item, _) => item,
                    _ => unreachable!("mir body guarantees a nested Array constant's own type is SizedArray"),
                };
                let stride = layout::total_bytes(item, pointer_bytes);
                for (i, element) in elements.iter().enumerate() {
                    self.write_const_element(blob, offset + i as u32 * stride, element, item);
                }
            }
            ConstValue::Struct(fields) => {
                let struct_type = match r#type {
                    ResolvedType::Struct(struct_type) => struct_type,
                    _ => unreachable!("a Struct constant's own type is always ResolvedType::Struct"),
                };
                let field_types: Vec<ResolvedType> =
                    struct_type.borrow().fields.iter().map(|(_, t, _)| t.clone()).collect();
                for (i, (value, field_type)) in fields.iter().zip(&field_types).enumerate() {
                    let field_offset = layout::field_byte_offset(&struct_type.borrow(), i, pointer_bytes);
                    self.write_const_element(blob, offset + field_offset, value, field_type);
                }
            }
            ConstValue::Enum { variant_index, tag, dynamic_fields, fields, .. } => {
                let cell = match r#type {
                    ResolvedType::Enum { cell, .. } => cell,
                    _ => unreachable!("an Enum constant's own type is always ResolvedType::Enum"),
                };
                let (tag_type, header, dynamic, body) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[*variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|((_, t, _), v)| (t.clone(), v.clone()))
                        .collect();
                    let dynamic: Vec<(u32, ResolvedType)> = (0..enum_type.dynamic_fields.len())
                        .map(|i| {
                            let field_offset = layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes);
                            (field_offset, enum_type.dynamic_fields[i].1.clone())
                        })
                        .collect();
                    let body: Vec<(u32, ResolvedType)> = (0..variant.fields.len())
                        .map(|i| {
                            let field_offset = layout::enum_body_field_offset(&enum_type, *variant_index, i, pointer_bytes);
                            (field_offset, variant.fields[i].1.clone())
                        })
                        .collect();
                    (enum_type.tag_type.clone(), header, dynamic, body)
                };

                self.write_const_element(blob, offset, &ConstValue::Number(*tag), &tag_type);
                let mut header_offset = offset + layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    self.write_const_element(blob, header_offset, value, field_type);
                    header_offset += layout::total_bytes(field_type, pointer_bytes);
                }
                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    self.write_const_element(blob, offset + field_offset, value, field_type);
                }
                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    self.write_const_element(blob, offset + field_offset, value, field_type);
                }
            }
            ConstValue::Union { field_index, value } => {
                let union_type = match r#type {
                    ResolvedType::Union(union_type) => union_type,
                    _ => unreachable!("a Union constant's own type is always ResolvedType::Union"),
                };
                let field_type = union_type.borrow().fields[*field_index].1.clone();
                self.write_const_element(blob, offset, value, &field_type);
            }
            ConstValue::Ref(inner) => {
                let pointee = match r#type {
                    ResolvedType::Pointer { pointee, .. } => pointee,
                    _ => unreachable!("a Ref constant's own type is always ResolvedType::Pointer"),
                };
                let inner_type = ref_pointee_type(inner, pointee);
                let inner_id = self.build_const_data(inner, &inner_type);
                blob.relocs.push((offset, inner_id));
            }
        }
    }

    /// Appends `value`'s canonical, unambiguous content bytes to `out`,
    /// purely for `data_symbol`'s naming -- the exact counterpart of
    /// `cranelift::hash_const_element`, algorithm-for-algorithm identical
    /// so both backends name identical constants identically (see there
    /// for why the *logical* tree is hashed rather than the physical
    /// buffer).
    fn hash_const_element(&mut self, out: &mut Vec<u8>, value: &ConstValue, r#type: &ResolvedType) {
        let pointer_bytes = self.pointer_bytes();
        match value {
            ConstValue::Number(number) => {
                let leaf = omega_analyzer::layout::leaves_of(r#type, pointer_bytes)[0];
                let leaf_bytes = leaf.bytes(pointer_bytes);
                let raw: u64 = match number {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) if leaf_bytes == 4 => (*v as f32).to_bits() as u64,
                    NumberValue::Float(v) => v.to_bits(),
                };
                out.extend_from_slice(&raw.to_le_bytes()[..leaf_bytes as usize]);
            }
            ConstValue::Bool(b) => out.push(*b as u8),
            ConstValue::Char(c) => out.extend_from_slice(&(*c as u32).to_le_bytes()),
            ConstValue::Str(s) => {
                out.extend_from_slice(&(s.len() as u64).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            ConstValue::Slice(nested) => {
                let item = match r#type {
                    ResolvedType::Slice { item, .. } => item,
                    _ => unreachable!("mir body guarantees a nested Slice constant's own type is Slice"),
                };
                out.extend_from_slice(&(nested.len() as u32).to_le_bytes());
                for element in nested {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Array(elements) => {
                let item = match r#type {
                    ResolvedType::SizedArray(item, _) => item,
                    _ => unreachable!("mir body guarantees a nested Array constant's own type is SizedArray"),
                };
                for element in elements {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Struct(fields) => {
                let struct_type = match r#type {
                    ResolvedType::Struct(struct_type) => struct_type,
                    _ => unreachable!("a Struct constant's own type is always ResolvedType::Struct"),
                };
                let field_types: Vec<ResolvedType> =
                    struct_type.borrow().fields.iter().map(|(_, t, _)| t.clone()).collect();
                for (value, field_type) in fields.iter().zip(&field_types) {
                    self.hash_const_element(out, value, field_type);
                }
            }
            ConstValue::Enum { variant_index, tag, dynamic_fields, fields, .. } => {
                let cell = match r#type {
                    ResolvedType::Enum { cell, .. } => cell,
                    _ => unreachable!("an Enum constant's own type is always ResolvedType::Enum"),
                };
                out.extend_from_slice(&(*variant_index as u32).to_le_bytes());
                let tag_bits: u64 = match tag {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) => v.to_bits(),
                };
                out.extend_from_slice(&tag_bits.to_le_bytes());
                let (dynamic_types, field_types) = {
                    let enum_type = cell.borrow();
                    let dynamic_types: Vec<ResolvedType> =
                        enum_type.dynamic_fields.iter().map(|(_, t, _)| t.clone()).collect();
                    let field_types: Vec<ResolvedType> =
                        enum_type.variants[*variant_index].fields.iter().map(|(_, t, _)| t.clone()).collect();
                    (dynamic_types, field_types)
                };
                for (value, field_type) in dynamic_fields.iter().zip(&dynamic_types) {
                    self.hash_const_element(out, value, field_type);
                }
                for (value, field_type) in fields.iter().zip(&field_types) {
                    self.hash_const_element(out, value, field_type);
                }
            }
            ConstValue::Union { field_index, value } => {
                let union_type = match r#type {
                    ResolvedType::Union(union_type) => union_type,
                    _ => unreachable!("a Union constant's own type is always ResolvedType::Union"),
                };
                out.extend_from_slice(&(*field_index as u32).to_le_bytes());
                let field_type = union_type.borrow().fields[*field_index].1.clone();
                self.hash_const_element(out, value, &field_type);
            }
            ConstValue::Ref(inner) => {
                let pointee = match r#type {
                    ResolvedType::Pointer { pointee, .. } => pointee,
                    _ => unreachable!("a Ref constant's own type is always ResolvedType::Pointer"),
                };
                self.hash_const_element(out, inner, &ref_pointee_type(inner, pointee));
            }
        }
    }

    /// A pointer load with an explicit alignment (the vtable slot load).
    fn aligned_load_ptr(
        &self,
        base: PointerValue<'ctx>,
        offset: u32,
        align: u32,
    ) -> PointerValue<'ctx> {
        let int_ty: BasicTypeEnum = if self.pointer_bytes() == 8 {
            self.context.i64_type().into()
        } else {
            self.context.i32_type().into()
        };
        let ptr = self.gep(base, int_ty.into_int_type().const_int(offset as u64, false));
        let value = self
            .builder
            .build_load(self.ptr_type(), ptr, "")
            .expect("load always succeeds");
        if let Some(inst) = value.as_instruction_value() {
            let _ = inst.set_alignment(align);
        }
        value.into_pointer_value()
    }
}
