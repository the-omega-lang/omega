use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use super::place::PlaceStorage;
use cranelift::prelude::{
    FloatCC, FunctionBuilder, InstBuilder, IntCC, MemFlags, StackSlotData, StackSlotKind, Value,
    types,
};
use cranelift_module::Module;
use omega_analyzer::checked::{CastKind, NumberValue};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ConstValue, NumericKind, ResolvedType};
use omega_hir::BinaryOp;
use omega_mir::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall,
    MirEnumConstruct, MirExpr, MirExprNode, MirFunctionCall, MirSlice, MirSpecCoerce,
    MirStructLiteral, MirUnionConstruct,
};

impl Codegen {
    pub(super) fn process_expr(
        &mut self,
        builder: &mut FunctionBuilder,
        node: MirExprNode,
    ) -> Vec<Value> {
        match node.kind {
            MirExpr::String(s) => self.emit_bytes(builder, s),
            MirExpr::ByteString(s) => self.emit_bytes(builder, s),
            MirExpr::Const(value) => self.emit_const_value(builder, &value, &node.r#type),

            MirExpr::FunctionCall(MirFunctionCall {
                callee,
                fn_type,
                args,
            }) => {
                // MIR guarantees a single resolved callee; codegen performs no overload selection.
                let fnaddr = self.process_expr(builder, *callee)[0];

                let fixed_count = fn_type.params.len();
                let mut ir_args = vec![];
                for (i, arg) in args.into_iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(builder, arg);
                    if fn_type.is_variadic
                        && i >= fixed_count
                        && let [v] = value.as_mut_slice()
                    {
                        *v = self.promote_variadic_arg(builder, *v, &arg_type);
                    }
                    ir_args.push(value);
                }
                let mut ir_args = ir_args.into_iter().flatten().collect::<Vec<_>>();

                let sret_slot = self.maybe_sret_arg(builder, &fn_type, &mut ir_args);
                let call = self.emit_call_indirect(builder, fnaddr, &fn_type, &ir_args);
                self.call_result(builder, &fn_type, sret_slot, call)
            }

            // Dynamic-spec coercion builds the data-pointer/vtable-pointer pair.
            MirExpr::SpecCoerce(MirSpecCoerce { base, slots }) => {
                let ResolvedType::SpecObject {
                    spec, type_args, ..
                } = &node.r#type
                else {
                    unreachable!("mir body guarantees a SpecCoerce's own type is SpecObject");
                };
                let spec = spec.clone();
                let type_args = type_args.clone();
                let ResolvedType::Pointer { pointee, .. } = &base.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's base is a plain pointer");
                };
                let concrete = (**pointee).clone();
                let data_ptr = self.process_expr(builder, *base)[0];
                let vtable_id = self.vtable_for(&concrete, &spec, &type_args, &slots);
                let global_value = self.module.declare_data_in_func(vtable_id, builder.func);
                let vtable_ptr = builder
                    .ins()
                    .global_value(self.pointer_type(), global_value);
                vec![data_ptr, vtable_ptr]
            }

            // Dynamic-spec calls load the resolved slot from the vtable and call it indirectly.
            MirExpr::DynamicCall(MirDynamicCall {
                base,
                slot_index,
                fn_type,
                args,
            }) => {
                let base_leaves = self.get_place_value(&base, builder);
                let [data_ptr, vtable_ptr] = base_leaves.as_slice() else {
                    panic!("mir body guarantees a SpecObject place has exactly 2 leaves");
                };
                let (data_ptr, vtable_ptr) = (*data_ptr, *vtable_ptr);

                let ptr_bytes = self.pointer_type().bytes();
                let slot_offset = slot_index as i32 * ptr_bytes as i32;
                let fnaddr = builder.ins().load(
                    self.pointer_type(),
                    MemFlags::new(),
                    vtable_ptr,
                    slot_offset,
                );

                let mut ir_args = vec![data_ptr];
                for arg in args {
                    ir_args.extend(self.process_expr(builder, arg));
                }

                let sret_slot = self.maybe_sret_arg(builder, &fn_type, &mut ir_args);
                let call = self.emit_call_indirect(builder, fnaddr, &fn_type, &ir_args);
                self.call_result(builder, &fn_type, sret_slot, call)
            }

            MirExpr::Number(value) => {
                // This path requires a scalar leaf; aggregate shapes were rejected/resolved earlier.
                let ir_type = node.r#type.cranelift_leaves(self)[0];
                let result = match value {
                    NumberValue::Signed(v) => builder.ins().iconst(ir_type, v),
                    NumberValue::Unsigned(v) => builder.ins().iconst(ir_type, v as i64),
                    NumberValue::Float(v) if ir_type == types::F32 => {
                        builder.ins().f32const(v as f32)
                    }
                    NumberValue::Float(v) => builder.ins().f64const(v),
                };
                vec![result]
            }

            MirExpr::Bool(b) => vec![builder.ins().iconst(types::I8, b as i64)],

            // `sizeof` is emitted as a target-sized compile-time integer constant.
            MirExpr::Sizeof(target_type) => {
                let size = layout::total_bytes(&target_type, self.pointer_bytes());
                vec![builder.ins().iconst(self.pointer_type(), size as i64)]
            }

            // Represent `char` as its 32-bit integer leaf.
            MirExpr::Char(c) => vec![builder.ins().iconst(types::I32, c as i64)],

            MirExpr::Place(place) => self.get_place_value(&place, builder),

            MirExpr::Assignment(MirAssignment { target, value }) => {
                let values = self.process_expr(builder, *value);
                // All assignment destinations flow through the same resolved-place store path.
                let (storage, _) = self.resolve_place_storage(&target, builder);
                self.store_scalars(builder, &storage, &values);
                values
            }

            MirExpr::AddressOf(MirAddressOf { place }) => {
                let (storage, _) = self.resolve_place_storage(&place, builder);
                vec![self.place_storage_address(builder, &storage)]
            }

            MirExpr::Negate(base) => {
                let is_float = matches!(
                    base.r#type.numeric_kind(self.pointer_bytes() * 8),
                    Some(NumericKind::Float(_))
                );
                let value = self.process_expr(builder, *base)[0];
                let result = if is_float {
                    builder.ins().fneg(value)
                } else {
                    builder.ins().ineg(value)
                };
                vec![result]
            }

            MirExpr::BitNot(base) => {
                let value = self.process_expr(builder, *base)[0];
                vec![builder.ins().bnot(value)]
            }

            MirExpr::BinaryOp(MirBinaryOp { op, left, right }) => {
                let kind = match &left.r#type {
                    ResolvedType::Char => NumericKind::Unsigned(32),
                    ResolvedType::Bool => NumericKind::Unsigned(8),
                    r#type => r#type
                        .numeric_kind(self.pointer_bytes() * 8)
                        .expect("mir body guarantees BinaryOp operands are numeric, char, or bool"),
                };
                let left = self.process_expr(builder, *left)[0];
                let right = self.process_expr(builder, *right)[0];
                // Rely on backend integer div/rem traps; semantic checks need not duplicate them here.
                let result = match (op, kind) {
                    (BinaryOp::Add, NumericKind::Float(_)) => builder.ins().fadd(left, right),
                    (BinaryOp::Add, _) => builder.ins().iadd(left, right),
                    (BinaryOp::Sub, NumericKind::Float(_)) => builder.ins().fsub(left, right),
                    (BinaryOp::Sub, _) => builder.ins().isub(left, right),
                    (BinaryOp::Mul, NumericKind::Float(_)) => builder.ins().fmul(left, right),
                    (BinaryOp::Mul, _) => builder.ins().imul(left, right),
                    (BinaryOp::Div, NumericKind::Float(_)) => builder.ins().fdiv(left, right),
                    (BinaryOp::Div, NumericKind::Signed(_)) => builder.ins().sdiv(left, right),
                    (BinaryOp::Div, NumericKind::Unsigned(_)) => builder.ins().udiv(left, right),
                    (BinaryOp::Rem, NumericKind::Signed(_)) => builder.ins().srem(left, right),
                    (BinaryOp::Rem, NumericKind::Unsigned(_)) => builder.ins().urem(left, right),
                    (BinaryOp::Rem, NumericKind::Float(_)) => {
                        unreachable!("mir body rejects '%' on float operands")
                    }
                    (BinaryOp::BitAnd, _) => builder.ins().band(left, right),
                    (BinaryOp::BitOr, _) => builder.ins().bor(left, right),
                    (BinaryOp::BitXor, _) => builder.ins().bxor(left, right),
                    (BinaryOp::Shl, _) => builder.ins().ishl(left, right),
                    (BinaryOp::Shr, NumericKind::Signed(_)) => builder.ins().sshr(left, right),
                    (BinaryOp::Shr, NumericKind::Unsigned(_)) => builder.ins().ushr(left, right),
                    (BinaryOp::Shr, NumericKind::Float(_)) => {
                        unreachable!("mir body rejects '>>' on float operands")
                    }
                    (cmp, NumericKind::Float(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => FloatCC::Equal,
                            BinaryOp::Ne => FloatCC::NotEqual,
                            BinaryOp::Lt => FloatCC::LessThan,
                            BinaryOp::Le => FloatCC::LessThanOrEqual,
                            BinaryOp::Gt => FloatCC::GreaterThan,
                            BinaryOp::Ge => FloatCC::GreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().fcmp(cc, left, right)
                    }
                    (cmp, NumericKind::Signed(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => IntCC::Equal,
                            BinaryOp::Ne => IntCC::NotEqual,
                            BinaryOp::Lt => IntCC::SignedLessThan,
                            BinaryOp::Le => IntCC::SignedLessThanOrEqual,
                            BinaryOp::Gt => IntCC::SignedGreaterThan,
                            BinaryOp::Ge => IntCC::SignedGreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().icmp(cc, left, right)
                    }
                    (cmp, NumericKind::Unsigned(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => IntCC::Equal,
                            BinaryOp::Ne => IntCC::NotEqual,
                            BinaryOp::Lt => IntCC::UnsignedLessThan,
                            BinaryOp::Le => IntCC::UnsignedLessThanOrEqual,
                            BinaryOp::Gt => IntCC::UnsignedGreaterThan,
                            BinaryOp::Ge => IntCC::UnsignedGreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().icmp(cc, left, right)
                    }
                };
                vec![result]
            }

            MirExpr::ArrayLiteral(MirArrayLiteral { elements, .. }) => {
                // Flatten array elements in source order into the aggregate leaf sequence.
                elements
                    .into_iter()
                    .flat_map(|e| self.process_expr(builder, e))
                    .collect()
            }

            MirExpr::EnumConstruct(MirEnumConstruct {
                variant_index,
                fields,
            }) => {
                // Build enums in scratch storage because payload layout is byte-addressed, not purely leaf-addressed.
                let ResolvedType::Enum { cell, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a construction's own type is its enum");
                };
                let cell = cell.clone();
                let pointer_bytes = self.pointer_bytes();
                // Copy layout facts before mutation to avoid holding the metadata-cell borrow.
                let (tag, tag_type, header, payload_offset, chunk_leaves, field_offsets) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[variant_index];
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
                                variant_index,
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
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    total,
                    shift,
                ));

                let tag_values =
                    self.emit_const_value(builder, &ConstValue::Number(tag), &tag_type);
                self.store_scalars(
                    builder,
                    &PlaceStorage::Slot { slot, offset: 0 },
                    &tag_values,
                );

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (r#type, value) in &header {
                    let const_values = self.emit_const_value(builder, value, r#type);
                    self.store_scalars(
                        builder,
                        &PlaceStorage::Slot { slot, offset },
                        &const_values,
                    );
                    offset += layout::total_bytes(r#type, pointer_bytes);
                }

                let mut chunk_offset = payload_offset;
                for leaf in chunk_leaves {
                    let chunk = super::leaf::cranelift_type(leaf, self.pointer_type());
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += leaf.bytes(pointer_bytes);
                }

                // Preserve source evaluation order even though fields are stored by resolved layout.
                for field in fields {
                    let field_offset = field_offsets[field.field_index];
                    let values = self.process_expr(builder, field.value);
                    self.store_scalars(
                        builder,
                        &PlaceStorage::Slot {
                            slot,
                            offset: field_offset,
                        },
                        &values,
                    );
                }

                self.load_scalars(
                    builder,
                    &PlaceStorage::Slot { slot, offset: 0 },
                    &node.r#type,
                )
            }

            MirExpr::StructLiteral(MirStructLiteral { fields }) => {
                // Evaluate aggregate initializers in source order; layout order affects storage only.
                let ResolvedType::Struct(struct_type) = &node.r#type else {
                    unreachable!("mir body guarantees a struct literal's own type is a struct");
                };
                let field_count = struct_type.borrow().fields.len();
                let mut per_field: Vec<Option<Vec<Value>>> = vec![None; field_count];
                for field in fields {
                    per_field[field.field_index] = Some(self.process_expr(builder, field.value));
                }
                per_field
                    .into_iter()
                    .map(|leaves| leaves.expect("mir body guarantees every field is initialized"))
                    .flatten()
                    .collect()
            }

            MirExpr::Slice(MirSlice {
                base,
                item_type,
                start,
                end,
                inclusive,
            }) => {
                let (storage, base_type) = self.resolve_place_storage(&base, builder);
                let ptr_type = self.pointer_type();

                // Range slicing derives the new data pointer and length from the original slice pair.
                let (data_ptr, full_len) = match &base_type {
                    ResolvedType::SizedArray(_, size) => {
                        let ptr = self.place_storage_address(builder, &storage);
                        let len = builder.ins().iconst(types::I32, *size as i64);
                        (ptr, len)
                    }
                    ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
                        let leaves = self.load_scalars(builder, &storage, &base_type);
                        (leaves[0], leaves[1])
                    }
                    ResolvedType::Array(_, _) => {
                        let leaves = self.load_scalars(builder, &storage, &base_type);
                        (leaves[0], builder.ins().iconst(types::I32, 0))
                    }
                    _ => unreachable!(
                        "mir body guarantees a slice's base is SizedArray/Slice/Str/Array"
                    ),
                };

                let elem_size = layout::total_bytes(&item_type, self.pointer_bytes()) as i64;

                let start_val = match start {
                    Some(e) => self.process_expr(builder, *e)[0],
                    None => builder.ins().iconst(types::I32, 0),
                };
                // Inclusive ranges add one element to the computed slice length.
                let end_val = match end {
                    Some(e) => {
                        let v = self.process_expr(builder, *e)[0];
                        if inclusive {
                            builder.ins().iadd_imm(v, 1)
                        } else {
                            v
                        }
                    }
                    None => full_len,
                };

                let mut start_ext = start_val;
                if builder.func.dfg.value_type(start_ext) != ptr_type {
                    start_ext = builder.ins().uextend(ptr_type, start_ext);
                }
                let elem_size_val = builder.ins().iconst(ptr_type, elem_size);
                let byte_offset = builder.ins().imul(start_ext, elem_size_val);
                let new_ptr = builder.ins().iadd(data_ptr, byte_offset);
                let new_len = builder.ins().isub(end_val, start_val);

                vec![new_ptr, new_len]
            }

            MirExpr::Cast(MirCast {
                kind,
                target_type,
                base,
            }) => {
                // Unsizing uses the source aggregate length, not a reconstructed backend guess.
                let unsize_len = match &base.r#type {
                    ResolvedType::Pointer { pointee, .. } => match pointee.as_ref() {
                        ResolvedType::SizedArray(_, size) => Some(*size),
                        _ => None,
                    },
                    _ => None,
                };
                // Reinterpretation preserves the full flattened value before rebuilding the destination shape.
                let base_leaves = self.process_expr(builder, *base);
                let target_ir = target_type.cranelift_leaves(self)[0];
                match kind {
                    CastKind::Reinterpret => base_leaves,
                    CastKind::DropLength => vec![base_leaves[0]],
                    CastKind::Unsize => {
                        let len = unsize_len
                            .expect("mir body guarantees Unsize's base is Pointer{SizedArray}");
                        let len_val = builder.ins().iconst(types::I32, len as i64);
                        vec![base_leaves[0], len_val]
                    }
                    CastKind::IntExtend { signed: true } => {
                        vec![builder.ins().sextend(target_ir, base_leaves[0])]
                    }
                    CastKind::IntExtend { signed: false } => {
                        vec![builder.ins().uextend(target_ir, base_leaves[0])]
                    }
                    CastKind::IntTruncate => vec![builder.ins().ireduce(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: true } => {
                        vec![builder.ins().fcvt_from_sint(target_ir, base_leaves[0])]
                    }
                    CastKind::IntToFloat { signed: false } => {
                        vec![builder.ins().fcvt_from_uint(target_ir, base_leaves[0])]
                    }
                    CastKind::FloatToInt { signed: true } => {
                        vec![builder.ins().fcvt_to_sint_sat(target_ir, base_leaves[0])]
                    }
                    CastKind::FloatToInt { signed: false } => {
                        vec![builder.ins().fcvt_to_uint_sat(target_ir, base_leaves[0])]
                    }
                    CastKind::FloatExtend => {
                        vec![builder.ins().fpromote(target_ir, base_leaves[0])]
                    }
                    CastKind::FloatTruncate => {
                        vec![builder.ins().fdemote(target_ir, base_leaves[0])]
                    }
                    // Spec narrowing preserves the data pointer and swaps in the resolved narrower vtable.
                    CastKind::SpecNarrow { slot_offset } => {
                        let byte_offset = slot_offset as i64 * self.pointer_bytes() as i64;
                        let vtable = builder.ins().iadd_imm(base_leaves[1], byte_offset);
                        vec![base_leaves[0], vtable]
                    }
                }
            }

            MirExpr::UnionConstruct(MirUnionConstruct {
                field_index: _,
                value,
            }) => {
                // Enum constants use the same scratch-storage layout path as runtime enum construction.
                let total = layout::total_bytes(&node.r#type, self.pointer_bytes());
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    total,
                    4,
                ));

                let mut chunk_offset = 0u32;
                for chunk in node.r#type.cranelift_leaves(self) {
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += chunk.bytes();
                }

                let values = self.process_expr(builder, *value);
                self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &values);

                self.load_scalars(
                    builder,
                    &PlaceStorage::Slot { slot, offset: 0 },
                    &node.r#type,
                )
            }
        }
    }
}
