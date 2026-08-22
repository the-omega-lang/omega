use super::Codegen;
use super::leaf;
use super::place::PlaceStorage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, PointerValue, ValueKind};
use omega_analyzer::checked::{CastKind, NumberValue};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{CallingConvention, ConstValue, NumericKind, ResolvedType};
use omega_hir::BinaryOp;
use omega_mir::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall,
    MirEnumConstruct, MirExpr, MirExprNode, MirFunctionCall, MirSlice, MirSpecCoerce,
    MirStructLiteral, MirUnionConstruct,
};


impl<'ctx> Codegen<'ctx> {
    pub(super) fn process_expr(&mut self, node: &MirExprNode) -> Vec<BasicValueEnum<'ctx>> {
        match &node.kind {
            MirExpr::String(s) | MirExpr::ByteString(s) => self.emit_bytes(s.clone()),
            MirExpr::Const(value) => self.emit_const_value(value, &node.r#type),

            MirExpr::InlineAsm(asm) => {
                self.process_inline_asm(asm);
                vec![]
            }

            MirExpr::FunctionCall(MirFunctionCall {
                callee,
                fn_type,
                args,
            }) => {
                // Dynamic callees are addresses and must be emitted as indirect calls.
                let fnaddr = self.process_expr(callee)[0].into_pointer_value();

                let fixed_count = fn_type.params.len();
                let mut ir_args: Vec<BasicValueEnum> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(arg);
                    // C default argument promotions are a `foreign(c)` source-language
                    // interoperability rule, not generic variadic-tail behavior -- a variadic
                    // `foreign(sysv64)` tail is passed using its actual lowered Omega types and
                    // left to LLVM's own register/stack classification.
                    if fn_type.is_variadic
                        && fn_type.calling_convention == CallingConvention::C
                        && i >= fixed_count
                    {
                        for v in value.iter_mut() {
                            *v = self.promote_variadic_value(*v, &arg_type);
                        }
                    }
                    ir_args.extend(value);
                }

                // Indirect returns prepend the caller-allocated sret destination.
                let sret_slot = self.needs_sret(&fn_type.return_type).then(|| {
                    let shift =
                        layout::stack_align_shift(layout::type_alignment(&fn_type.return_type));
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
                call.set_call_convention(crate::abi::llvm_calling_convention(
                    fn_type.calling_convention,
                ));

                if matches!(*fn_type.return_type, ResolvedType::Void | ResolvedType::Never) {
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
                        // LLVM returns multiple leaves as one aggregate value that must be unpacked.
                        let leaves = leaf::llvm_leaves(
                            self.context,
                            &fn_type.return_type,
                            self.pointer_bytes(),
                        );
                        if leaves.len() > 1 {
                            (0..leaves.len())
                                .map(|i| {
                                    self.builder
                                        .build_extract_value(
                                            value.into_struct_value(),
                                            i as u32,
                                            "",
                                        )
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
                    ResolvedType::SpecObject {
                        spec, type_args, ..
                    } => (spec.clone(), type_args.clone()),
                    _ => unreachable!("mir body guarantees a SpecCoerce's own type is SpecObject"),
                };
                let concrete = match &base.r#type {
                    ResolvedType::Pointer { pointee, .. } => (**pointee).clone(),
                    _ => unreachable!("mir body guarantees a SpecCoerce's base is a plain pointer"),
                };
                let vtable = self.vtable_for(&concrete, &spec, &type_args, slots);
                vec![data_ptr.into(), vtable.as_pointer_value().into()]
            }

            MirExpr::DynamicCall(MirDynamicCall {
                base,
                slot_index,
                fn_type,
                args,
            }) => {
                let base_leaves = self.get_place_value(base);
                let data_ptr = base_leaves[0].into_pointer_value();
                let vtable_ptr = base_leaves[1].into_pointer_value();

                let fnaddr = self.aligned_load_ptr(
                    vtable_ptr,
                    *slot_index as u32 * self.pointer_bytes(),
                    self.pointer_bytes(),
                );

                let mut ir_args: Vec<BasicValueEnum> = vec![data_ptr.into()];
                for arg in args {
                    ir_args.extend(self.process_expr(arg));
                }
                let sret_slot = self.needs_sret(&fn_type.return_type).then(|| {
                    let shift =
                        layout::stack_align_shift(layout::type_alignment(&fn_type.return_type));
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

                if matches!(*fn_type.return_type, ResolvedType::Void | ResolvedType::Never) {
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
                        let leaves = leaf::llvm_leaves(
                            self.context,
                            &fn_type.return_type,
                            self.pointer_bytes(),
                        );
                        if leaves.len() > 1 {
                            (0..leaves.len())
                                .map(|i| {
                                    self.builder
                                        .build_extract_value(
                                            value.into_struct_value(),
                                            i as u32,
                                            "",
                                        )
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
                let raw_leaf =
                    omega_analyzer::layout::leaves_of(&node.r#type, self.pointer_bytes())[0];
                vec![self.scalar_const(raw_leaf, value)]
            }

            MirExpr::Bool(b) => vec![
                self.context
                    .i8_type()
                    .const_int(u64::from(*b), false)
                    .into(),
            ],

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
                    PlaceStorage::Values(_) => {
                        unreachable!(
                            "writes to SSA-backed places must be materialized before emission"
                        )
                    }
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
                vec![
                    self.builder
                        .build_not(value, "bnot")
                        .expect("bnot always succeeds")
                        .as_basic_value_enum(),
                ]
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
                // Normalize pointer leaves to pointer-width integers for integer-domain operations.
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
                        .build_int_unsigned_div(
                            left.into_int_value(),
                            right.into_int_value(),
                            "udiv",
                        )
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Rem, NumericKind::Signed(_)) => self
                        .builder
                        .build_int_signed_rem(left.into_int_value(), right.into_int_value(), "srem")
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Rem, NumericKind::Unsigned(_)) => self
                        .builder
                        .build_int_unsigned_rem(
                            left.into_int_value(),
                            right.into_int_value(),
                            "urem",
                        )
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
                        .build_right_shift(
                            left.into_int_value(),
                            right.into_int_value(),
                            true,
                            "sshr",
                        )
                        .unwrap()
                        .as_basic_value_enum(),
                    (BinaryOp::Shr, NumericKind::Unsigned(_)) => self
                        .builder
                        .build_right_shift(
                            left.into_int_value(),
                            right.into_int_value(),
                            false,
                            "ushr",
                        )
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
                            .build_float_compare(
                                pred,
                                left.into_float_value(),
                                right.into_float_value(),
                                "fcmp",
                            )
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
                        // Normalize pointer leaves before integer comparison.
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
                // Cast the integer-domain result back when the expression type is a pointer.
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
                    .flat_map(|leaves| {
                        leaves.expect("mir body guarantees every field is initialized")
                    })
                    .collect()
            }

            MirExpr::EnumConstruct(MirEnumConstruct {
                variant_index,
                fields,
            }) => {
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
                        .map(|(resolved_field, value)| {
                            (resolved_field.r#type.clone(), value.clone())
                        })
                        .collect();
                    let field_offsets: Vec<u32> = (0..enum_type.dynamic_fields.len())
                        .map(|i| layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes))
                        .chain((0..variant.fields.len()).map(|i| {
                            layout::enum_body_field_offset(
                                &enum_type,
                                *variant_index,
                                i,
                                pointer_bytes,
                            )
                        }))
                        .collect();
                    let payload_offset = layout::enum_payload_offset(&enum_type, pointer_bytes);
                    let chunk_leaves = layout::payload_chunks(layout::enum_payload_bytes(
                        &enum_type,
                        enum_type.layout.pack,
                        pointer_bytes,
                    ));
                    (
                        variant.tag,
                        enum_type.tag_type.clone(),
                        header,
                        payload_offset,
                        chunk_leaves,
                        field_offsets,
                    )
                };

                let shift = layout::stack_align_shift(layout::type_alignment(&node.r#type));
                let total = layout::total_bytes(&node.r#type, pointer_bytes);
                let slot = self.entry_alloca(total, 1u32 << shift, "enum");

                let tag_values = self.emit_const_value(&ConstValue::Number(tag), &tag_type);
                self.store_scalars(&slot, 0, &tag_values, layout::type_alignment(&tag_type));

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (r#type, value) in &header {
                    let const_values = self.emit_const_value(value, r#type);
                    self.store_scalars(
                        &slot,
                        offset,
                        &const_values,
                        layout::type_alignment(r#type),
                    );
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

            MirExpr::UnionConstruct(MirUnionConstruct {
                field_index: _,
                value,
            }) => {
                let total = layout::total_bytes(&node.r#type, self.pointer_bytes());
                let slot = self.entry_alloca(total, 16, "union");

                let mut chunk_offset = 0u32;
                for raw_leaf in
                    omega_analyzer::layout::leaves_of(&node.r#type, self.pointer_bytes())
                {
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

            MirExpr::Slice(MirSlice {
                base,
                item_type,
                start,
                end,
                inclusive,
            }) => {
                let (storage, base_type, _align) = self.resolve_place_storage(base);

                let (data_ptr, full_len) = match &base_type {
                    ResolvedType::SizedArray(_, size) => {
                        let ptr = self.place_storage_address(&storage);
                        let len: BasicValueEnum = self
                            .context
                            .i32_type()
                            .const_int(*size as u64, false)
                            .into();
                        (ptr, len)
                    }
                    ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
                        let leaves = self.load_scalars(
                            &storage,
                            &base_type,
                            layout::type_alignment(&base_type),
                        );
                        (leaves[0].into_pointer_value(), leaves[1])
                    }
                    ResolvedType::Array(_, _) => {
                        let leaves = self.load_scalars(
                            &storage,
                            &base_type,
                            layout::type_alignment(&base_type),
                        );
                        (
                            leaves[0].into_pointer_value(),
                            self.context.i32_type().const_int(0, false).into(),
                        )
                    }
                    _ => unreachable!(
                        "mir body guarantees a slice's base is SizedArray/Slice/Str/Array"
                    ),
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
                                .build_int_add(
                                    v,
                                    self.context.i32_type().const_int(1, false),
                                    "inc",
                                )
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
                    .build_int_z_extend(
                        start_val.into_int_value(),
                        ptr_int.into_int_type(),
                        "start",
                    )
                    .unwrap();
                let byte_offset = self
                    .builder
                    .build_int_mul(
                        start_ext,
                        ptr_int.into_int_type().const_int(elem_size as u64, true),
                        "byteoff",
                    )
                    .unwrap();
                let new_ptr = self.gep(data_ptr, byte_offset);
                let new_len = self
                    .builder
                    .build_int_sub(
                        end_val.into_int_value(),
                        start_val.into_int_value(),
                        "slicelen",
                    )
                    .unwrap();

                vec![new_ptr.into(), new_len.into()]
            }

            MirExpr::Cast(MirCast {
                kind,
                target_type,
                base,
            }) => {
                let unsize_len = match &base.r#type {
                    ResolvedType::Pointer { pointee, .. } => match pointee.as_ref() {
                        ResolvedType::SizedArray(_, size) => Some(*size),
                        _ => None,
                    },
                    _ => None,
                };
                let base_leaves = self.process_expr(base);
                let target_ir =
                    leaf::llvm_leaves(self.context, target_type, self.pointer_bytes())[0];
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
                        let len = unsize_len
                            .expect("mir body guarantees Unsize's base is Pointer{SizedArray}");
                        vec![
                            base_leaves[0],
                            self.context.i32_type().const_int(len as u64, false).into(),
                        ]
                    }
                    // Preserve pointer provenance until the cast actually requires integer bits.
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
                        vec![
                            self.builder
                                .build_signed_int_to_float(
                                    base_int,
                                    target_ir.into_float_type(),
                                    "sitofp",
                                )
                                .unwrap()
                                .as_basic_value_enum(),
                        ]
                    }
                    CastKind::IntToFloat { signed: false } => {
                        let base_int = self.to_int_operand(base_leaves[0]);
                        vec![
                            self.builder
                                .build_unsigned_int_to_float(
                                    base_int,
                                    target_ir.into_float_type(),
                                    "uitofp",
                                )
                                .unwrap()
                                .as_basic_value_enum(),
                        ]
                    }
                    // Omega's float-to-int cast is saturating, not trapping (docs/language/strings-casts-arrays-and-slices.md).
                    CastKind::FloatToInt { signed } => {
                        let from = base_leaves[0].into_float_value().get_type();
                        let to = if target_ir.is_pointer_type() {
                            leaf::size_type(self.context, self.pointer_bytes())
                        } else {
                            target_ir.into_int_type()
                        };
                        let family = if *signed {
                            "llvm.fptosi.sat"
                        } else {
                            "llvm.fptoui.sat"
                        };
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
                    CastKind::FloatExtend => vec![
                        self.builder
                            .build_float_ext(
                                base_leaves[0].into_float_value(),
                                target_ir.into_float_type(),
                                "fpext",
                            )
                            .unwrap()
                            .as_basic_value_enum(),
                    ],
                    CastKind::FloatTruncate => vec![
                        self.builder
                            .build_float_trunc(
                                base_leaves[0].into_float_value(),
                                target_ir.into_float_type(),
                                "fptrunc",
                            )
                            .unwrap()
                            .as_basic_value_enum(),
                    ],
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

    pub(super) fn scalar_const(
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
            // `usize`/`isize` use pointer-width bits but remain integer values, not pointers.
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
                let float_ty = if raw_leaf == Leaf::F32 {
                    self.context.f32_type()
                } else {
                    self.context.f64_type()
                };
                match value {
                    NumberValue::Float(v) => float_ty.const_float(*v).into(),
                    _ => unreachable!("a float literal is always a NumberValue::Float"),
                }
            }
        }
    }

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

    fn bool_result(&self, value: inkwell::values::IntValue<'ctx>) -> BasicValueEnum<'ctx> {
        self.builder
            .build_int_z_extend(value, self.context.i8_type(), "tobool8")
            .expect("zext always succeeds")
            .as_basic_value_enum()
    }

    pub(super) fn to_i1(
        &self,
        value: inkwell::values::IntValue<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
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

    fn promote_variadic_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        arg_type: &ResolvedType,
    ) -> BasicValueEnum<'ctx> {
        match crate::abi::variadic_promotion(arg_type, self.target) {
            Some(NumericKind::Float(_)) => self
                .builder
                .build_float_ext(
                    value.into_float_value(),
                    self.context.f64_type(),
                    "fpromote",
                )
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

    pub(super) fn get_place_value(
        &mut self,
        place: &omega_mir::MirPlace,
    ) -> Vec<BasicValueEnum<'ctx>> {
        // Function references are code pointers, not addressable data places.
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
